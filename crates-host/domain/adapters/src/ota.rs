//! Обновление прошивки поверх `embassy-boot`: разделы `DFU` (куда пишется
//! новый образ) и `BOOTLOADER_STATE` (где лежит решение bootloader'а) — два
//! `NorFlash`, откуда они взялись, адаптеру всё равно. Прошивка строит их из
//! линкерных символов (`crates-cross/bsp/src/ota.rs`), host-тест — из памяти.
//!
//! Что здесь есть: подготовка раздела под образ, запись и чтение кусков,
//! состояние bootloader'а, подтверждение образа, пометка обмена и — под фичей
//! `signed` — проверка подписи. Чего нет: ни порядка проверок при применении
//! (`domain::update`), ни буферизации до слова (`domain::download`) — это
//! логика, а здесь только доступ к разделам.
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
    pub fn erase_size(&self) -> u32 {
        DFU::ERASE_SIZE as u32
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
    /// образа в проекте с подписью — `domain::update::apply_signed`,
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
        DFU::WRITE_SIZE as u32
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
        let page = DFU::ERASE_SIZE as u32;
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
impl<DFU: NorFlash, STATE: NorFlash> Signed<DFU, STATE> {
    /// `running_version` — упакованная версия этого образа
    /// (`domain::firmware::pack`), `public_key` — корень доверия; нули означают
    /// «ключ не подставлен» и отвергаются логикой до криптографии.
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
    fn mark_updated(&mut self) -> Result<(), Self::Error> {
        self.inner.mark_updated()
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
    fn verify_and_mark_updated(
        &mut self,
        signature: &[u8; 64],
        len: u32,
    ) -> Result<(), Self::Error> {
        let key = self.public_key;
        self.inner.verify_and_mark_updated(&key, signature, len)
    }
}

#[cfg(test)]
mod tests {
    use super::{Error, Updater};
    use crate::mem_flash::MemFlash;
    use embedded_storage::nor_flash::NorFlashErrorKind;
    use ports::FirmwareUpdate;

    /// Четыре страницы по 256 байт, слово 8; ACTIVE (= capacity) — три
    /// страницы, как требует запас на обмен.
    type Dfu = MemFlash<1024, 256, 8>;
    /// Раздел состояния: страница на магию плюс журнал прогресса.
    type State = MemFlash<256, 256, 8>;

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

    #[test]
    fn reports_the_flash_geometry() {
        let mut u = updater();
        assert_eq!(u.write_granularity(), 8);
        assert_eq!(u.erase_size(), 256);
        assert_eq!(u.capacity().expect("вместимость"), 768);
    }
}
