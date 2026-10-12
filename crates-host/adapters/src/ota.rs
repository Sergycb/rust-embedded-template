//! Обновление прошивки поверх `embassy-boot`: разделы `DFU` (куда пишется
//! новый образ) и `BOOTLOADER_STATE` (где лежит решение bootloader'а) — два
//! `NorFlash`, откуда они взялись, адаптеру всё равно. Прошивка строит их из
//! линкерных символов (`crates-cross/bsp/src/ota.rs`), host-тест — из памяти.
//!
//! Что здесь есть: подготовка раздела под образ, запись и чтение кусков,
//! состояние bootloader'а, подтверждение образа, пометка обмена и — под фичей
//! `signed` — проверка подписи. Чего нет: ни порядка проверок при применении
//! (`domain::ota::update`), ни буферизации до слова (`domain::ota::download`) —
//! это логика, а здесь только доступ к разделам.
//!
//! # Про подтверждение: без него обновление живёт один запуск
//!
//! `embassy-boot` меняет разделы местами и ждёт, что новый образ отметится
//! как работоспособный (`mark_booted`). Не отметился — на следующем сбросе
//! bootloader откатит обновление обратно. Отсюда два следствия: подтверждать
//! надо **из нового образа**, и момент подтверждения — это и есть определение
//! «прошивка работает» (первая строка `main` подтверждает любой образ, который
//! смог стартовать; правильное место — там, где устройство доказало
//! работоспособность). Пока образ не подтверждён, отказывают и `prepare`, и
//! `write`: в `DFU` тогда лежит образ, куда откатываться, и затирать его нельзя.

use embassy_boot::{
    AlignedBuffer, BlockingFirmwareState, BlockingFirmwareUpdater, FirmwareUpdaterConfig, State,
};
use embedded_storage::nor_flash::{NorFlash, NorFlashErrorKind};
use ports::FirmwareUpdate;
// Нужен только `Signed` (под фичей): безусловный импорт в проекте без подписи
// давал `unused_imports` на каждой сборке — корневой lint этого не видит,
// он всегда включает `adapters/signed`.
#[cfg(feature = "signed")]
use ports::VerifyError;

// Тип возвращается портом, поэтому называть его должно быть чем — иначе
// пользователю пришлось бы объявлять прямую зависимость на `embassy-boot`
// только ради имени в сигнатуре своей функции.
pub use embassy_boot::FirmwareUpdaterError as Error;

/// Самое широкое слово записи среди STM32 — тридцать два байта (H7).
///
/// `embassy-boot` требует буфер ровно в `STATE::WRITE_SIZE` байт с
/// выравниванием флеша; держится буфер на максимум, а апдейтеру отдаётся срез
/// нужной длины — так адаптер остаётся generic, не зная слова конкретного чипа.
pub const MAX_WRITE_SIZE: usize = 32;

/// Доступ к разделам обновления.
pub struct Updater<DFU: NorFlash, STATE: NorFlash> {
    dfu: DFU,
    state: STATE,
    /// Буфер под одно слово записи во flash: `embassy-boot` пишет через него
    /// состояние, и выравнивание должно быть флешевым (`AlignedBuffer` — 32).
    aligned: AlignedBuffer<MAX_WRITE_SIZE>,
    /// Самый длинный образ, который доедет до устройства: размер `ACTIVE`, а
    /// не `DFU`. Приёмник по построению больше активного на страницу-две
    /// (`assert_partitions` в embassy-boot), а обмен копирует ровно `ACTIVE`;
    /// образ между ними прошёл бы все проверки и приехал обрезанным.
    capacity: u32,
}

impl<DFU: NorFlash, STATE: NorFlash> core::fmt::Debug for Updater<DFU, STATE> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Updater").finish_non_exhaustive()
    }
}

impl<DFU: NorFlash, STATE: NorFlash> Updater<DFU, STATE> {
    /// `capacity` — размер раздела, в который образ будет исполняться
    /// (`ACTIVE`), см. поле.
    pub fn new(dfu: DFU, state: STATE, capacity: u32) -> Self {
        const {
            assert!(
                STATE::WRITE_SIZE <= MAX_WRITE_SIZE,
                "слово записи раздела состояния шире буфера адаптера (MAX_WRITE_SIZE)"
            );
        }
        Self {
            dfu,
            state,
            aligned: AlignedBuffer([0; MAX_WRITE_SIZE]),
            capacity,
        }
    }

    /// Гранулярность стирания раздела в байтах — размер страницы или сектора.
    ///
    /// Не в порту: домену эта величина не нужна (он мыслит длиной образа), а
    /// вот target-тестам — да: `prepare` округляет длину вверх именно до неё.
    ///
    /// # Panics
    ///
    /// Паникует, если `DFU::ERASE_SIZE` превышает `u32::MAX`. На STM32 размер
    /// сектора ограничен аппаратными пределами (максимум 256 Кбайт), поэтому
    /// на практике это невозможно.
    pub fn erase_size(&self) -> u32 {
        u32::try_from(DFU::ERASE_SIZE).expect("ERASE_SIZE не должен превышать u32::MAX")
    }

    /// Апдейтер держит ссылки и на разделы, и на буфер, поэтому хранить его
    /// полем нельзя — структура вышла бы самоссылающейся. Собирается он
    /// дёшево (две ссылки и срез), так что каждая операция делает себе свой.
    fn updater(&mut self) -> BlockingFirmwareUpdater<'_, &mut DFU, &mut STATE> {
        let config = FirmwareUpdaterConfig {
            dfu: &mut self.dfu,
            state: &mut self.state,
        };
        BlockingFirmwareUpdater::new(config, &mut self.aligned.0[..STATE::WRITE_SIZE])
    }

    /// `embassy-boot` прячет `BlockingFirmwareUpdater::mark_updated` под
    /// `#[cfg(not(feature = "_verify"))]` — с включённой проверкой подписи он
    /// не хочет давать пометить обмен без проверки. Порт же объявляет
    /// `mark_updated` безусловно (путь без подписи, проверок там нет
    /// намеренно — см. `ports`), а `Signed` его делегирует; поэтому пометка
    /// идёт через `BlockingFirmwareState`, который под фичей не спрятан и
    /// делает ровно то же (`set_magic(SWAP_MAGIC)`). Защита от непроверенного
    /// образа в проекте с подписью — `domain::ota::update::apply_signed`,
    /// единственный путь, которым им пользуется `app`.
    fn state(&mut self) -> BlockingFirmwareState<'_, &mut STATE> {
        BlockingFirmwareState::new(&mut self.state, &mut self.aligned.0[..STATE::WRITE_SIZE])
    }

    /// Занят ли раздел `DFU` тем, что терять нельзя — см.
    /// [`FirmwareUpdate::is_busy`]. `embassy-boot` в `verify_booted` `Revert`
    /// разрешает, и это его право: он отвечает на «можно ли писать вообще», а
    /// не на «не жалко ли того, что лежит в разделе». Здесь правило строже
    /// намеренно: README советует переносить `mark_booted()` туда, где
    /// устройство доказало работоспособность, то есть окно `Revert` в реальном
    /// проекте живёт долго, и начатая в нём передача стирала бы образ отката.
    fn dfu_is_busy(&mut self) -> Result<bool, Error> {
        Ok(matches!(
            self.updater().get_state()?,
            State::Swap | State::Revert
        ))
    }

    #[cfg(feature = "signed")]
    fn verify_and_mark_updated(
        &mut self,
        public_key: &[u8; 32],
        signature: &[u8; 64],
        len: u32,
    ) -> Result<(), Error> {
        self.updater()
            .verify_and_mark_updated(public_key, signature, len)
    }
}

/// Порт домена. Собственных методов с теми же именами у `Updater` нет
/// намеренно: они перекрыли бы трейтовые при вызове, и приложение работало бы
/// с конкретным типом мимо порта — то есть мимо той границы, ради которой
/// порт заведён.
impl<DFU: NorFlash, STATE: NorFlash> FirmwareUpdate for Updater<DFU, STATE> {
    type Error = Error;

    fn write_granularity(&mut self) -> u32 {
        u32::try_from(DFU::WRITE_SIZE).expect("WRITE_SIZE не должен превышать u32::MAX")
    }

    fn capacity(&mut self) -> Result<u32, Self::Error> {
        Ok(self.capacity)
    }

    fn is_busy(&mut self) -> Result<bool, Self::Error> {
        self.dfu_is_busy()
    }

    /// Стирает столько страниц раздела `DFU`, сколько нужно образу длиной
    /// `len`. `prepare_update()` из `embassy-boot` здесь не годится: он
    /// стирает раздел целиком, а это и время (стирание идёт в критической
    /// секции, на крупных секторах — секунды), и лишний износ флеша.
    fn prepare(&mut self, len: u32) -> Result<(), Self::Error> {
        if self.dfu_is_busy()? {
            return Err(Error::BadState);
        }
        // Негодная длина — отказ ДО стирания: стерев раздел, мы уничтожаем
        // образ, в который bootloader откатывается. Ноль отвергается по той же
        // причине: стирать нечего, а следующая запись легла бы в нестёртую
        // память.
        if len == 0 || len > self.capacity {
            return Err(Error::Flash(NorFlashErrorKind::OutOfBounds));
        }
        // Длина округляется ВВЕРХ до страницы: стереть половину страницы флеш
        // не умеет, а оставить её нестёртой — значит потерять хвост образа.
        // За границу раздела округление не уйдёт: `DFU` больше `ACTIVE`
        // (= `capacity`) минимум на страницу; `saturating_mul` — страховка на
        // случай, если проверку однажды ослабят.
        let page = u32::try_from(DFU::ERASE_SIZE).expect("ERASE_SIZE не должен превышать u32::MAX");
        let end = len.div_ceil(page).saturating_mul(page);
        self.dfu.erase(0, end)?;
        Ok(())
    }

    /// Пишет кусок прямо в раздел, а не через `write_firmware` из
    /// `embassy-boot`: тот стирает сектор, «если он не тот, что стирали в
    /// прошлый раз», и помнит это полем апдейтера — объекта, который здесь
    /// создаётся заново на каждый вызов. У свежего «в прошлый раз» пусто, и
    /// каждая запись стирала бы сектор, в который пишет (проверено на живой
    /// плате: две записи в один сектор, первая читалась как `0xFF`).
    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        // Запрет на запись, пока раздел занят, — та самая проверка, с которой
        // начинался `write_firmware` (`verify_booted`), только строже на одно
        // состояние. Прямая запись её не делает, а без неё приложение затёрло
        // бы образ, в который bootloader собирается откатиться.
        if self.dfu_is_busy()? {
            return Err(Error::BadState);
        }
        self.dfu.write(offset, data)?;
        Ok(())
    }

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.updater().read_dfu(offset, buf)
    }

    fn mark_booted(&mut self) -> Result<(), Self::Error> {
        self.updater().mark_booted()
    }

    fn mark_updated(&mut self) -> Result<(), Self::Error> {
        self.state().mark_updated()
    }
}

/// [`Updater`] вместе с тем, что нужно проверке подписи: версией текущей
/// прошивки и открытым ключом. Обёртка, а не поля в `Updater`: без подписи
/// они ничего не значат, а тридцать шесть байт RAM «на всякий случай» — не
/// то, что стоит раздавать каждому проекту.
#[cfg(feature = "signed")]
pub struct Signed<DFU: NorFlash, STATE: NorFlash> {
    inner: Updater<DFU, STATE>,
    version: u32,
    public_key: [u8; 32],
}

#[cfg(feature = "signed")]
impl<DFU: NorFlash, STATE: NorFlash> core::fmt::Debug for Signed<DFU, STATE> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Signed").finish_non_exhaustive()
    }
}

#[cfg(feature = "signed")]
impl<DFU: NorFlash, STATE: NorFlash> Signed<DFU, STATE> {
    /// `running_version` — упакованная версия этого образа
    /// (`domain::ota::version::pack`), `public_key` — корень доверия; нули
    /// означают «ключ не подставлен» и отвергаются логикой до криптографии.
    pub fn new(inner: Updater<DFU, STATE>, running_version: u32, public_key: [u8; 32]) -> Self {
        Self {
            inner,
            version: running_version,
            public_key,
        }
    }

    /// См. [`Updater::erase_size`].
    pub fn erase_size(&self) -> u32 {
        self.inner.erase_size()
    }
}

#[cfg(feature = "signed")]
impl<DFU: NorFlash, STATE: NorFlash> FirmwareUpdate for Signed<DFU, STATE> {
    type Error = Error;

    fn write_granularity(&mut self) -> u32 {
        self.inner.write_granularity()
    }
    fn capacity(&mut self) -> Result<u32, Self::Error> {
        self.inner.capacity()
    }
    fn is_busy(&mut self) -> Result<bool, Self::Error> {
        self.inner.is_busy()
    }
    fn prepare(&mut self, len: u32) -> Result<(), Self::Error> {
        self.inner.prepare(len)
    }
    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        self.inner.write(offset, data)
    }
    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.inner.read(offset, buf)
    }
    fn mark_booted(&mut self) -> Result<(), Self::Error> {
        self.inner.mark_booted()
    }
    /// В проекте с подписью единственный путь к обмену разделов —
    /// `domain::ota::update::apply_signed` (проверка длины, занятости, версии,
    /// ключа и самой подписи). Порт объявляет `mark_updated` безусловно,
    /// поэтому метод остаётся, но отвечает `BadState` — тот же отказ, которым
    /// `embassy-boot` отвечает на попытку обмена в негодном состоянии, и тот
    /// же, что даёт `verify_and_mark_updated` при неподтверждённом образе. Так
    /// восстанавливается гарантия старого `bsp::ota`, где у подписанного
    /// варианта метода `mark_updated` не было вовсе.
    fn mark_updated(&mut self) -> Result<(), Self::Error> {
        Err(Error::BadState)
    }
}

#[cfg(feature = "signed")]
impl<DFU: NorFlash, STATE: NorFlash> ports::SignedFirmwareUpdate for Signed<DFU, STATE> {
    fn running_version(&self) -> u32 {
        self.version
    }

    fn public_key(&self) -> &[u8; 32] {
        &self.public_key
    }

    /// Проверка читает весь раздел и считает по нему хеш — заметное время
    /// (портируемая реализация ed25519 — порядка сотни миллионов тактов).
    /// Случается это один раз перед перезагрузкой.
    ///
    /// `Signature(_)` у embassy-boot — и не сошедшаяся подпись, и ключ,
    /// который не разбирается как точка кривой; для отправителя это одно и
    /// то же «подпись негодная».
    fn verify_and_mark_updated(
        &mut self,
        signature: &[u8; 64],
        len: u32,
    ) -> Result<(), VerifyError<Self::Error>> {
        let key = self.public_key;
        self.inner
            .verify_and_mark_updated(&key, signature, len)
            .map_err(|err| match err {
                Error::Signature(_) => VerifyError::BadSignature,
                other => VerifyError::Flash(other),
            })
    }
}

/// Что приложение оставляет bootloader'у о принятом образе: длину и подпись.
///
/// Лежит в запасной странице `DFU` — по смещению `capacity` (размер `ACTIVE`).
/// `DFU` по построению на страницу больше `ACTIVE` (`assert_partitions` в
/// `embassy-boot`), и эта страница нужна только самому обмену как черновик:
/// до него она свободна, а первый же шаг обмена (`ACTIVE[last]` →
/// `DFU[last + 1]`) её затирает — к этому моменту bootloader запись уже
/// проверил. Своё место в разделе, а не отдельная запись: подпись должна
/// пережить перезагрузку между приёмом и проверкой, а `STATE` `embassy-boot`
/// стирает сам.
#[cfg(feature = "signed")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PendingImage {
    /// Длина образа в байтах — сколько начальных байт `DFU` покрывает подпись.
    pub len: u32,
    /// Подпись ed25519 над SHA-512 образа — так её ставит `cargo xtask build`.
    pub signature: [u8; 64],
}

/// Магия записи — отличает её от стёртой страницы и от остатков чужих данных.
#[cfg(feature = "signed")]
const PENDING_MAGIC: [u8; 8] = *b"OTA-SIG1";

/// Размер записи во флеше: магия (8), длина (4), резерв (4), подпись (64) —
/// 80 байт, дополненные до кратного [`MAX_WRITE_SIZE`]: запись идёт одним
/// куском, и на любом STM32 она обязана быть кратна слову.
#[cfg(feature = "signed")]
pub const PENDING_RECORD_LEN: usize = 96;

#[cfg(feature = "signed")]
impl PendingImage {
    fn to_bytes(self) -> [u8; PENDING_RECORD_LEN] {
        let mut out = [0xFF; PENDING_RECORD_LEN];
        out[..8].copy_from_slice(&PENDING_MAGIC);
        out[8..12].copy_from_slice(&self.len.to_le_bytes());
        out[16..80].copy_from_slice(&self.signature);
        out
    }

    fn from_bytes(bytes: &[u8; PENDING_RECORD_LEN]) -> Option<Self> {
        if bytes[..8] != PENDING_MAGIC {
            return None;
        }
        let mut len = [0; 4];
        len.copy_from_slice(&bytes[8..12]);
        let mut signature = [0; 64];
        signature.copy_from_slice(&bytes[16..80]);
        Some(Self {
            len: u32::from_le_bytes(len),
            signature,
        })
    }
}

/// Пишет запись о принятом образе в запасную страницу `DFU` (смещение
/// `capacity`). Страница к этому моменту должна быть стёрта — это делает
/// `prepare` проекта с подписью.
///
/// # Errors
///
/// Ошибка флеша, в том числе запись в нестёртую память.
#[cfg(feature = "signed")]
pub fn write_pending<DFU: NorFlash>(
    dfu: &mut DFU,
    capacity: u32,
    image: PendingImage,
) -> Result<(), Error> {
    const {
        assert!(
            PENDING_RECORD_LEN.is_multiple_of(DFU::WRITE_SIZE),
            "запись о принятом образе не кратна слову записи"
        );
    }
    dfu.write(capacity, &image.to_bytes())?;
    Ok(())
}

/// Что решил bootloader об обновлении — см. [`screen_update`].
#[cfg(feature = "signed")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Screening {
    /// Нового запроса на обмен нет: обычный старт, обмен, прерванный на
    /// середине, или откат неподтверждённого образа. Проверять нечего —
    /// `embassy-boot` делает своё.
    NothingToCheck,
    /// Образ в `DFU` подписан верно — обмен пусть идёт.
    Approved,
    /// Образ отвергнут, запрос на обмен снят (`STATE` → `Boot`): загрузится
    /// прежний образ.
    Rejected(Rejection),
}

/// Почему образ в `DFU` отвергнут.
#[cfg(feature = "signed")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// Обмен запрошен, а записи о принятом образе нет — образ положили мимо
    /// `write_pending`, то есть без подписи.
    NoRecord,
    /// Длина в записи — ноль или больше `ACTIVE`.
    Length,
    /// Открытый ключ — нули, то есть не подставлен. Отдельной проверкой, а не
    /// «с нулями всё равно не сойдётся»: нулевой ключ ed25519 — точка малого
    /// порядка, подпись под него подделывается, и `salty` такие не отсеивает.
    NoPublicKey,
    /// Ключ не разбирается как точка кривой или подпись не сошлась.
    Signature,
}

/// Проверка нового образа bootloader'ом — один раз, перед обменом.
///
/// Проверяет только **новый** запрос: магия `Swap` в `STATE` и пустой журнал
/// прогресса. Та же магия держится и после обмена, пока новый образ не
/// подтвердится, — тогда `embassy-boot` откатывает его, и запись к этому
/// времени уже затёрта обменом; проверять там нечего, а «отвергнуть» значило бы
/// сорвать откат. Журнал читается без знания стёртого значения (у части чипов
/// это `0x00`): при магии `Swap` слово 1 («журнал действителен») всегда
/// стёрто, поэтому «обмен не начинался» — это слово 2 (первая отметка
/// прогресса), побайтно равное слову 1. Формат журнала — внутренний у
/// `embassy-boot` (doc у `BootLoader::state`), и привязку к нему держит
/// host-тест с настоящим обменом `embassy-boot`.
///
/// Отказ снимает запрос: в `STATE` пишется магия `Boot` (`mark_booted`), и
/// следующий за этим `BootLoader::prepare` грузит прежний образ.
///
/// Время: SHA-512 всего образа и одна проверка ed25519 — порядка сотни
/// миллионов тактов в портируемой реализации. Сторож к этому моменту не
/// запущен: проверка — один неделимый вызов, покормить его внутри нечем.
///
/// # Errors
///
/// Ошибка флеша при чтении разделов или при снятии запроса.
///
/// # Panics
///
/// Если `STATE::WRITE_SIZE` шире [`MAX_WRITE_SIZE`] или не помещается в
/// `u32` — на STM32 слово записи не больше 32 байт, так что на практике нет.
#[cfg(feature = "signed")]
pub fn screen_update<DFU: NorFlash, STATE: NorFlash>(
    dfu: &mut DFU,
    state: &mut STATE,
    capacity: u32,
    public_key: &[u8; 32],
) -> Result<Screening, Error> {
    let mut aligned = AlignedBuffer([0; MAX_WRITE_SIZE]);
    let word = &mut aligned.0[..STATE::WRITE_SIZE];
    if BlockingFirmwareState::new(&mut *state, &mut *word).get_state()? != State::Swap {
        return Ok(Screening::NothingToCheck);
    }
    let mut validity = [0; MAX_WRITE_SIZE];
    let mut first_step = [0; MAX_WRITE_SIZE];
    let size = u32::try_from(STATE::WRITE_SIZE).expect("WRITE_SIZE не должен превышать u32::MAX");
    state.read(size, &mut validity[..STATE::WRITE_SIZE])?;
    state.read(2 * size, &mut first_step[..STATE::WRITE_SIZE])?;
    if validity != first_step {
        return Ok(Screening::NothingToCheck);
    }

    let verdict = check_pending(dfu, capacity, public_key)?;
    if let Err(rejection) = verdict {
        BlockingFirmwareState::new(&mut *state, &mut aligned.0[..STATE::WRITE_SIZE])
            .mark_booted()?;
        return Ok(Screening::Rejected(rejection));
    }
    Ok(Screening::Approved)
}

/// Сама проверка записи и подписи — без решения, что делать с отказом.
#[cfg(feature = "signed")]
fn check_pending<DFU: NorFlash>(
    dfu: &mut DFU,
    capacity: u32,
    public_key: &[u8; 32],
) -> Result<Result<(), Rejection>, Error> {
    /// Сколько байт образа читается за раз на хеш.
    const CHUNK: u32 = 256;
    let mut record = [0; PENDING_RECORD_LEN];
    dfu.read(capacity, &mut record)?;
    let Some(pending) = PendingImage::from_bytes(&record) else {
        return Ok(Err(Rejection::NoRecord));
    };
    if pending.len == 0 || pending.len > capacity {
        return Ok(Err(Rejection::Length));
    }
    if public_key.iter().all(|byte| *byte == 0) {
        return Ok(Err(Rejection::NoPublicKey));
    }

    let mut digest = salty::Sha512::new();
    let mut chunk = [0; CHUNK as usize];
    let mut offset = 0;
    while offset < pending.len {
        let len = CHUNK.min(pending.len - offset);
        let bytes = &mut chunk[..len as usize];
        dfu.read(offset, bytes)?;
        digest.update(bytes);
        offset += len;
    }
    let message = digest.finalize();

    let Ok(key) = salty::PublicKey::try_from(public_key) else {
        return Ok(Err(Rejection::Signature));
    };
    let signature = salty::Signature::from(&pending.signature);
    if key.verify(&message, &signature).is_ok() {
        Ok(Ok(()))
    } else {
        Ok(Err(Rejection::Signature))
    }
}

#[cfg(test)]
mod tests {
    #[cfg(feature = "signed")]
    use super::Signed;
    use super::{Error, Updater};
    use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};
    use ports::FirmwareUpdate;

    /// Флеш в памяти с семантикой NOR: стирание страницами по 256 байт в
    /// `0xFF`, запись словами по 8 байт и только «единицы в нули». Готового
    /// мока `NorFlash` в экосистеме нет (`embedded-hal-mock` флеш не
    /// покрывает), поэтому фейк свой и ровно под эти тесты.
    #[derive(Debug)]
    struct MemFlash<const SIZE: usize> {
        mem: Vec<u8>,
    }

    impl<const SIZE: usize> MemFlash<SIZE> {
        fn erased() -> Self {
            Self::filled(0xFF)
        }

        fn filled(byte: u8) -> Self {
            Self {
                mem: vec![byte; SIZE],
            }
        }

        fn check(offset: u32, len: usize, unit: usize) -> Result<(), NorFlashErrorKind> {
            if offset as usize + len > SIZE {
                return Err(NorFlashErrorKind::OutOfBounds);
            }
            if !(offset as usize).is_multiple_of(unit) || !len.is_multiple_of(unit) {
                return Err(NorFlashErrorKind::NotAligned);
            }
            Ok(())
        }
    }

    impl<const SIZE: usize> ErrorType for MemFlash<SIZE> {
        type Error = NorFlashErrorKind;
    }

    impl<const SIZE: usize> ReadNorFlash for MemFlash<SIZE> {
        const READ_SIZE: usize = 1;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            Self::check(offset, bytes.len(), 1)?;
            let start = offset as usize;
            bytes.copy_from_slice(&self.mem[start..start + bytes.len()]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            SIZE
        }
    }

    impl<const SIZE: usize> NorFlash for MemFlash<SIZE> {
        const WRITE_SIZE: usize = 8;
        const ERASE_SIZE: usize = 256;

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            Self::check(from, (to - from) as usize, Self::ERASE_SIZE)?;
            self.mem[from as usize..to as usize].fill(0xFF);
            Ok(())
        }

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            Self::check(offset, bytes.len(), Self::WRITE_SIZE)?;
            for (cell, byte) in self.mem[offset as usize..].iter_mut().zip(bytes) {
                *cell &= *byte;
            }
            Ok(())
        }
    }

    /// Четыре страницы по 256 байт, слово 8; ACTIVE (= capacity) — три
    /// страницы, как требует запас на обмен.
    type Dfu = MemFlash<1024>;
    /// Раздел состояния: страница на магию плюс журнал прогресса.
    type State = MemFlash<256>;

    fn updater() -> Updater<Dfu, State> {
        Updater::new(Dfu::erased(), State::erased(), 768)
    }

    fn with_state(magic: u8) -> Updater<Dfu, State> {
        let mut state = State::erased();
        state.mem[..8].fill(magic);
        Updater::new(Dfu::erased(), state, 768)
    }

    #[test]
    fn prepare_erases_only_the_pages_the_image_needs() {
        let mut u = Updater::new(Dfu::filled(0x00), State::erased(), 768);

        u.prepare(257).expect("стереть под образ");

        assert!(
            u.dfu.mem[..512].iter().all(|b| *b == 0xFF),
            "две страницы стёрты"
        );
        assert!(
            u.dfu.mem[512..].iter().all(|b| *b == 0x00),
            "третья не тронута"
        );
    }

    #[test]
    fn prepare_refuses_zero_and_oversized_lengths_before_erasing() {
        for len in [0, 769] {
            let mut u = Updater::new(Dfu::filled(0x00), State::erased(), 768);

            let refused = u.prepare(len);

            assert!(
                matches!(refused, Err(Error::Flash(NorFlashErrorKind::OutOfBounds))),
                "{len}"
            );
            assert!(
                u.dfu.mem.iter().all(|b| *b == 0x00),
                "раздел не должен быть стёрт"
            );
        }
    }

    #[test]
    fn write_and_prepare_refuse_while_the_partition_holds_a_rollback_image() {
        // 0xF0 — SWAP_MAGIC, 0xC0 — REVERT_MAGIC (`embassy-boot`, lib.rs).
        for magic in [0xF0, 0xC0] {
            let mut u = with_state(magic);

            assert!(u.is_busy().expect("состояние"), "{magic:#x}");
            assert!(matches!(u.prepare(8), Err(Error::BadState)));
            assert!(matches!(u.write(0, &[0; 8]), Err(Error::BadState)));
            assert!(
                u.read(0, &mut [0; 8]).is_ok(),
                "чтение занятого раздела разрешено"
            );
        }
    }

    #[test]
    fn a_booted_state_is_not_busy() {
        // 0xD0 — BOOT_MAGIC; стёртый раздел читается как Boot тоже.
        assert!(!with_state(0xD0).is_busy().expect("состояние"));
        assert!(!updater().is_busy().expect("состояние"));
    }

    #[test]
    fn writes_and_reads_back_after_prepare() {
        let mut u = updater();
        u.prepare(16).expect("подготовка");

        u.write(0, &[0xA5; 8]).expect("первая запись");
        u.write(8, &[0x5A; 8])
            .expect("вторая запись в тот же сектор");
        let mut back = [0; 16];
        u.read(0, &mut back).expect("чтение");

        assert_eq!(&back[..8], &[0xA5; 8]);
        assert_eq!(&back[8..], &[0x5A; 8]);
    }

    #[test]
    fn mark_updated_makes_the_partition_busy_and_mark_booted_frees_it() {
        let mut u = updater();

        u.mark_updated().expect("пометить обмен");
        assert!(u.is_busy().expect("состояние"));

        u.mark_booted().expect("подтвердить");
        assert!(!u.is_busy().expect("состояние"));
    }

    /// Порт объявляет `mark_updated` безусловно, но у `Signed` он не должен
    /// давать обменять разделы мимо проверки подписи — единственный путь
    /// туда лежит через `domain::ota::update::apply_signed`.
    #[cfg(feature = "signed")]
    #[test]
    fn signed_mark_updated_always_refuses() {
        let mut signed = Signed::new(updater(), 1, [7; 32]);

        assert!(matches!(signed.mark_updated(), Err(Error::BadState)));
        assert!(
            !signed.is_busy().expect("состояние"),
            "состояние не тронуто"
        );
    }

    /// Подпись, которая не может сойтись (ключ и подпись — константы), доходит
    /// до порта своим вариантом, а не как отказ флеша.
    #[cfg(feature = "signed")]
    #[test]
    fn signed_reports_a_bad_signature_as_such() {
        use ports::{SignedFirmwareUpdate, VerifyError};
        let mut signed = Signed::new(updater(), 1, [7; 32]);
        signed.prepare(64).expect("стереть под образ");
        signed.write(0, &[0xA5; 64]).expect("записать образ");

        assert!(matches!(
            signed.verify_and_mark_updated(&[9; 64], 64),
            Err(VerifyError::BadSignature)
        ));
        assert!(
            !signed.is_busy().expect("состояние"),
            "негодная подпись не должна помечать обмен"
        );
    }

    #[test]
    fn reports_the_flash_geometry() {
        let mut u = updater();
        assert_eq!(u.write_granularity(), 8);
        assert_eq!(u.erase_size(), 256);
        assert_eq!(u.capacity().expect("вместимость"), 768);
    }

    /// Проверка нового образа bootloader'ом (`screen_update`): настоящие ключи
    /// `salty` и — для отката — настоящий обмен `embassy-boot` над теми же
    /// фейками, что и выше.
    #[cfg(feature = "signed")]
    mod screening {
        use embassy_boot::{
            BlockingFirmwareState, BootLoader, BootLoaderConfig, State as BootState,
        };

        use super::{Dfu, MemFlash, State};
        use crate::ota::{PendingImage, Rejection, Screening, screen_update, write_pending};

        /// `ACTIVE` — три страницы, как `capacity` у `updater()` выше.
        const CAPACITY: u32 = 768;
        const SEED: [u8; 32] = [42; 32];

        fn public_key() -> [u8; 32] {
            salty::Keypair::from(&SEED).public.to_bytes()
        }

        /// Подпись так, как её ставит `cargo xtask build`: ed25519 над SHA-512
        /// образа.
        fn sign(image: &[u8]) -> [u8; 64] {
            let digest = salty::Sha512::new().updated(image).finalize();
            salty::Keypair::from(&SEED).sign(&digest).to_bytes()
        }

        /// `DFU` с образом и записью о нём и `STATE` с запрошенным обменом —
        /// то, что оставляет приложение перед перезагрузкой.
        fn pending(image: &[u8], record: PendingImage) -> (Dfu, State) {
            let mut dfu = Dfu::erased();
            dfu.mem[..image.len()].copy_from_slice(image);
            write_pending(&mut dfu, CAPACITY, record).expect("запись о принятом образе");
            let mut state = State::erased();
            BlockingFirmwareState::new(&mut state, &mut [0; 8])
                .mark_updated()
                .expect("запросить обмен");
            (dfu, state)
        }

        fn state_of(state: &mut State) -> BootState {
            BlockingFirmwareState::new(state, &mut [0; 8])
                .get_state()
                .expect("состояние")
        }

        fn image() -> Vec<u8> {
            (0..600u32).map(|i| (i % 251) as u8).collect()
        }

        #[test]
        fn a_correctly_signed_image_is_approved_and_the_swap_stays_requested() {
            let image = image();
            let (mut dfu, mut state) = pending(
                &image,
                PendingImage {
                    len: 600,
                    signature: sign(&image),
                },
            );

            let screening = screen_update(&mut dfu, &mut state, CAPACITY, &public_key());

            assert_eq!(screening.expect("проверка"), Screening::Approved);
            assert_eq!(state_of(&mut state), BootState::Swap);
        }

        #[test]
        fn a_bad_image_is_rejected_and_the_swap_request_is_withdrawn() {
            let image = image();
            let mut tampered = image.clone();
            tampered[10] ^= 1;
            let good = sign(&image);
            let cases = [
                (
                    tampered.clone(),
                    600,
                    good,
                    public_key(),
                    Rejection::Signature,
                ),
                (image.clone(), 0, good, public_key(), Rejection::Length),
                (
                    image.clone(),
                    CAPACITY + 1,
                    good,
                    public_key(),
                    Rejection::Length,
                ),
                (image.clone(), 600, good, [0; 32], Rejection::NoPublicKey),
            ];
            for (bytes, len, signature, key, expected) in cases {
                let (mut dfu, mut state) = pending(&bytes, PendingImage { len, signature });

                let screening = screen_update(&mut dfu, &mut state, CAPACITY, &key);

                assert_eq!(
                    screening.expect("проверка"),
                    Screening::Rejected(expected),
                    "{expected:?}"
                );
                assert_eq!(state_of(&mut state), BootState::Boot, "{expected:?}");
            }
        }

        /// Обмен запросили, минуя запись (образ без подписи), — отказ, а не
        /// «проверять нечего».
        #[test]
        fn a_swap_request_without_a_record_is_rejected() {
            let mut dfu = Dfu::erased();
            let mut state = State::erased();
            BlockingFirmwareState::new(&mut state, &mut [0; 8])
                .mark_updated()
                .expect("запросить обмен");

            let screening = screen_update(&mut dfu, &mut state, CAPACITY, &public_key());

            assert_eq!(
                screening.expect("проверка"),
                Screening::Rejected(Rejection::NoRecord)
            );
            assert_eq!(state_of(&mut state), BootState::Boot);
        }

        #[test]
        fn nothing_is_checked_without_a_swap_request() {
            let mut dfu = Dfu::erased();
            let mut state = State::erased();

            let screening = screen_update(&mut dfu, &mut state, CAPACITY, &public_key());

            assert_eq!(screening.expect("проверка"), Screening::NothingToCheck);
        }

        /// Главное: после настоящего обмена `embassy-boot` магия в `STATE` та же
        /// (`Swap`, пока образ не подтверждён), а запись затёрта. Проверка
        /// обязана это узнать по журналу и не трогать `STATE` — иначе следующий
        /// сброс не откатил бы неподтверждённый образ. Заодно тест держит
        /// привязку к внутреннему формату журнала `embassy-boot`.
        #[test]
        fn after_the_swap_the_pending_revert_is_left_alone() {
            let image = image();
            let (mut dfu, mut state) = pending(
                &image,
                PendingImage {
                    len: 600,
                    signature: sign(&image),
                },
            );
            assert_eq!(
                screen_update(&mut dfu, &mut state, CAPACITY, &public_key()).expect("проверка"),
                Screening::Approved
            );
            let mut active = MemFlash::<768>::filled(0x11);

            let mut boot = BootLoader::new(BootLoaderConfig {
                active: &mut active,
                dfu: &mut dfu,
                state: &mut state,
            });
            assert_eq!(
                boot.prepare_boot(&mut [0; 8]).expect("обмен"),
                BootState::Swap
            );
            assert_eq!(&active.mem[..600], &image[..], "образ переехал в ACTIVE");

            let screening = screen_update(&mut dfu, &mut state, CAPACITY, &public_key());

            assert_eq!(screening.expect("проверка"), Screening::NothingToCheck);
            assert_eq!(
                state_of(&mut state),
                BootState::Swap,
                "откат должен остаться запрошенным"
            );
        }
    }
}
