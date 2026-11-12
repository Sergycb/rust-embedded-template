//! Обновление прошивки поверх `embassy-boot`: разделы `DFU` (куда пишется
//! новый образ) и `BOOTLOADER_STATE` (где лежит решение bootloader'а) — два
//! `NorFlash`, откуда они взялись, адаптеру всё равно. Режет их плата
//! (`bsp::flash`) по символам `memory.x` и отдаёт готовыми —
//! [`ports::Partitions`]; в host-тесте это разделы в памяти.
//!
//! Что здесь есть: подготовка раздела под образ, запись и чтение кусков,
//! состояние bootloader'а, подтверждение образа, пометка обмена — это
//! [`Updater`](crate::ota::Updater) над **асинхронными** разделами: за ними
//! может стоять и внутренний флеш, и внешняя микросхема на SPI. Чего нет: ни
//! решения bootloader'а и проверки подписи нового образа (`ota::boot`, над
//! блокирующими разделами: у bootloader'а нет исполнителя), ни порядка
//! проверок при применении, ни буферизации до слова — это логика
//! (`ota::transfer`), а здесь только доступ к разделам.
//!
//! Тип конкретный, без трейта: реализация одна, и приём, применение и узел
//! зовут её прямо. Отказы, которые надо проверить на хосте (запись для
//! bootloader'а, подтверждение, обмен), разыгрывает флеш под ней
//! (`ota::fake`).
//!
//! # Про подтверждение: без него обновление живёт один запуск
//!
//! `embassy-boot` меняет разделы местами и ждёт, что новый образ отметится
//! как работоспособный (`mark_booted`). Не отметился — на следующем сбросе
//! bootloader откатит обновление обратно. Отсюда два следствия: подтверждать
//! надо **из нового образа**, и момент подтверждения — это и есть определение
//! «прошивка работает» (узел OTA при старте подтверждает любой образ, который
//! смог до него дойти; правильное место — там, где устройство доказало
//! работоспособность). Пока образ не подтверждён, отказывают и `prepare`, и
//! `write`: в `DFU` тогда лежит образ, куда откатываться, и затирать его нельзя.

use embassy_boot::{
    AlignedBuffer, FirmwareState, FirmwareUpdater, FirmwareUpdaterConfig, FirmwareUpdaterError,
    State,
};
use embedded_storage::nor_flash::{NorFlashError, NorFlashErrorKind};
use embedded_storage_async::nor_flash::NorFlash;
use ports::{MAX_WRITE_SIZE, Partitions, PendingImage};

/// Почему разделы обновления не дали операцию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum UpdaterError {
    /// Отказ флеша, в том числе длина или смещение за разделом.
    Flash(NorFlashErrorKind),
    /// Операция запрещена в нынешнем состоянии: в `DFU` лежит образ, который
    /// терять нельзя (см. [`Updater::is_busy`]).
    BadState,
}

impl From<FirmwareUpdaterError> for UpdaterError {
    fn from(error: FirmwareUpdaterError) -> Self {
        match error {
            FirmwareUpdaterError::Flash(kind) => Self::Flash(kind),
            FirmwareUpdaterError::BadState => Self::BadState,
            // Подпись проверяет `verify_and_mark_updated`, а его адаптер не
            // зовёт (подпись проверяет bootloader, `ota::boot`): такой ошибки
            // не бывает. Пришла бы — отказ устройства, а не паника узла.
            FirmwareUpdaterError::Signature(_) => Self::Flash(NorFlashErrorKind::Other),
        }
    }
}

/// Доступ к разделам обновления `P` — [`Partitions`] платы.
///
/// `prepare` стирает всё, что обмен перенесёт в `ACTIVE` (`DFU[..capacity]`),
/// и запасную страницу за ним — туда `ota::apply` кладёт запись для
/// bootloader'а ([`PendingImage`]); оставь её нестёртой — запись легла бы
/// поверх остатков прошлого обмена.
///
/// `capacity` разделов — самый длинный образ, который доедет до устройства:
/// размер `ACTIVE`, а не `DFU` (docs/ota.md, «Приём образа: стирание отдельно
/// от записи»).
pub struct Updater<P> {
    parts: P,
    /// Буфер под одно слово записи во flash: `embassy-boot` пишет через него
    /// состояние, и выравнивание должно быть флешевым (`AlignedBuffer` — 32).
    aligned: AlignedBuffer<MAX_WRITE_SIZE>,
}

impl<P> core::fmt::Debug for Updater<P> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Updater").finish_non_exhaustive()
    }
}

impl<D: NorFlash, S: NorFlash> Updater<Partitions<D, S>> {
    /// Адаптер над разделами `parts`.
    ///
    /// # Panics
    ///
    /// Паникует, если `capacity` не кратна странице стирания `DFU`
    /// или за ней не помещается запасная страница: запись для bootloader'а
    /// легла бы мимо раздела. Раскладку считает генерация, поэтому падение
    /// случится на первом же запуске.
    pub fn new(parts: Partitions<D, S>) -> Self {
        const {
            assert!(
                S::WRITE_SIZE <= MAX_WRITE_SIZE,
                "слово записи раздела состояния шире буфера адаптера (MAX_WRITE_SIZE)"
            );
            // Слово `DFU` — то, до чего приём (`ota::transfer`) копит хвост
            // образа в буфере на `MAX_WRITE_SIZE`: шире буфер не вместит, а
            // нулевое делило бы на ноль.
            assert!(
                D::WRITE_SIZE > 0 && D::WRITE_SIZE <= MAX_WRITE_SIZE,
                "слово записи DFU — от 1 до MAX_WRITE_SIZE байт"
            );
            assert!(
                PendingImage::LEN.is_multiple_of(D::WRITE_SIZE),
                "запись для bootloader'а не кратна слову записи DFU"
            );
            // Тогда и `capacity` (кратна странице, проверено ниже) кратна
            // слову: добитый до слова образ не длиннее `capacity`.
            assert!(
                D::ERASE_SIZE.is_multiple_of(D::WRITE_SIZE),
                "страница стирания DFU не кратна слову записи"
            );
        }
        let capacity = parts.capacity;
        let page = D::ERASE_SIZE as u64;
        assert!(
            u64::from(capacity).is_multiple_of(page),
            "размер ACTIVE ({capacity}) не кратен странице стирания DFU ({page}) — проверьте memory.x"
        );
        assert!(
            u64::from(capacity) + page <= parts.dfu.capacity() as u64,
            "в DFU ({}) не помещается ACTIVE ({capacity}) и запасная страница ({page}) — проверьте memory.x",
            parts.dfu.capacity()
        );
        Self {
            parts,
            aligned: AlignedBuffer([0; MAX_WRITE_SIZE]),
        }
    }

    /// Кратность, которой обязана быть длина каждого [`write`](Self::write),
    /// — слово программирования `DFU`, от 1 до [`MAX_WRITE_SIZE`] байт
    /// (проверено в [`new`](Self::new)).
    ///
    /// Канал доставки об этом не знает и отдаёт пакеты какой угодно длины,
    /// поэтому кто-то должен копить их до кратности;
    /// `ota::Transfer` делает это за вас.
    ///
    /// # Panics
    ///
    /// Не паникует: слово не шире `MAX_WRITE_SIZE` (`new`).
    pub fn write_granularity(&self) -> u32 {
        u32::try_from(D::WRITE_SIZE).expect("слово DFU не шире MAX_WRITE_SIZE")
    }

    /// Гранулярность стирания раздела в байтах — размер страницы или сектора.
    ///
    /// Логике эта величина не нужна (она мыслит длиной образа), а вот
    /// target-тестам — да: по ней они находят последнюю страницу `ACTIVE` и
    /// запасную за ней (`prepare` стирает постранично).
    ///
    /// # Panics
    ///
    /// Паникует, если `DFU::ERASE_SIZE` превышает `u32::MAX`. На STM32 размер
    /// сектора ограничен аппаратными пределами (максимум 256 Кбайт), поэтому
    /// на практике это невозможно.
    pub fn erase_size(&self) -> u32 {
        u32::try_from(D::ERASE_SIZE).expect("ERASE_SIZE не должен превышать u32::MAX")
    }

    /// Самый длинный образ, который устройство способно принять и запустить:
    /// размер `ACTIVE`, а не `DFU`.
    ///
    /// У схемы с обменом разделов приёмник по построению больше активного, и
    /// образ, влезший в приёмник, может не поместиться туда, где ему
    /// предстоит исполняться. Нужна до начала приёма: длину образа сообщает
    /// недоверенный канал, и сверить её стоит раньше, чем стирать флеш.
    pub fn capacity(&self) -> u32 {
        self.parts.capacity
    }

    /// Отпечаток раскладки памяти этой прошивки (`ports::layout_fingerprint`
    /// по `ports::LAYOUT_SYMBOLS`) — с ним сверяется отпечаток в хвосте
    /// принятого образа. Его считает плата (`bsp::layout`), адаптер отдаёт как
    /// получил.
    pub fn layout(&self) -> u32 {
        self.parts.layout
    }

    /// В разделе обновления лежит образ, который терять нельзя.
    ///
    /// `STATE` в `Swap` — два случая, и оба — «трогать раздел нельзя»: обмен
    /// запрошен и ждёт сброса (в `DFU` — новый образ, bootloader перенесёт
    /// его на ближайшем сбросе) или обмен уже прошёл, а новый образ себя не
    /// подтвердил (в `DFU` — предыдущая, заведомо рабочая прошивка, к ней
    /// bootloader откатится на ближайшем сбросе). Пока это так,
    /// [`prepare`](Self::prepare) и [`write`](Self::write) отказывают;
    /// логика применения спрашивает отдельно, чтобы отличить этот отказ от
    /// отката по версии.
    ///
    /// `Revert` — откат уже случился: в `ACTIVE` прежний образ, в `DFU` —
    /// отвергнутый. Беречь в `DFU` нечего, и `embassy-boot` в `verify_booted`
    /// писать в нём разрешает, но здесь `Revert` тоже считается занятым: он
    /// держится, только пока прежний образ себя не подтвердил (`mark_booted`
    /// узла OTA переводит `STATE` в `Boot`), то есть подтверждение не прошло —
    /// принимать обновление на устройстве в таком состоянии незачем.
    ///
    /// # Errors
    ///
    /// Отказ флеша при чтении состояния.
    pub async fn is_busy(&mut self) -> Result<bool, UpdaterError> {
        Ok(matches!(
            self.updater().get_state().await?,
            State::Swap | State::Revert
        ))
    }

    /// Готовит раздел обновления к приёму образа длиной `len` — стирает его.
    ///
    /// Стирает `DFU[..capacity]` — всё, что обмен `embassy-boot` перенесёт в
    /// `ACTIVE`, а не только страницы под образ длиной `len`, — и запасную
    /// страницу под запись для bootloader'а. Подпись покрывает только `len`
    /// байт, а обмен копирует раздел целиком: нестёртый хвост с остатками
    /// прошлых загрузок (в том числе чужого, отвергнутого bootloader'ом
    /// образа) уехал бы в `ACTIVE` непроверенным. Худшее время от этого не
    /// растёт — образ во весь `ACTIVE` стирал столько же; дольше становится
    /// только приём короткого образа (docs/ota.md, «Приём образа: стирание
    /// отдельно от записи»).
    /// `len` нужна для отказа до стирания: ноль и длина сверх
    /// [`capacity`](Self::capacity) — отказ, а не «сколько влезет», потому что
    /// стирание уничтожает образ, в который устройство откатывается.
    ///
    /// Зовётся один раз перед первым [`write`](Self::write), а не при каждом
    /// куске (почему стирание не в записи — docs/ota.md, «Приём образа:
    /// стирание отдельно от записи»).
    ///
    /// # Это самый долгий вызов во всём обновлении
    ///
    /// Стирание занимает от сотен миллисекунд (страницы по паре килобайт) до
    /// нескольких секунд (сектор в четверть мегабайта). Стирает по странице и
    /// уступает исполнитель после каждой: флеш платы блокирующий
    /// (`BlockingAsync`), и стирание всего диапазона одним вызовом держало бы
    /// все задачи прошивки, пока идёт стирание. Короче одной страницы пауза не
    /// станет — её стирание неделимо (сектор F4 — 128 KiB, до 2 с).
    // {%- if graph == "true" %}
    /// Отсюда граница по таймауту сторожа — одна страница (docs/ota.md,
    /// «Приём образа: стирание отдельно от записи»).
    // {%- endif %}
    ///
    /// # Errors
    ///
    /// `BadState` — раздел занят ([`is_busy`](Self::is_busy)); `Flash` —
    /// негодная длина (`OutOfBounds`) или отказ стирания.
    pub async fn prepare(&mut self, len: u32) -> Result<(), UpdaterError> {
        if self.is_busy().await? {
            return Err(UpdaterError::BadState);
        }
        // Негодная длина — отказ ДО стирания: стерев раздел, мы уничтожаем
        // образ, в который bootloader откатывается. Ноль отвергается по той же
        // причине: стирать нечего, а следующая запись легла бы в нестёртую
        // память.
        if len == 0 || len > self.parts.capacity {
            return Err(UpdaterError::Flash(NorFlashErrorKind::OutOfBounds));
        }
        // `capacity` кратна странице, и за ней помещается запасная — это
        // проверил `new`, поэтому `end` не выходит за раздел.
        let page = self.erase_size();
        let end = self.parts.capacity + page;
        for from in (0..end).step_by(page as usize) {
            self.parts
                .dfu
                .erase(from, from + page)
                .await
                .map_err(|error| UpdaterError::Flash(error.kind()))?;
            embassy_futures::yield_now().await;
        }
        Ok(())
    }

    /// Пишет кусок нового образа по смещению от начала раздела обновления.
    ///
    /// Требует, чтобы раздел был подготовлен [`prepare`](Self::prepare):
    /// большинство семейств на нестёртую запись отвечают ошибкой
    /// программирования, а F2/F4/F7 принимают её **молча**, оставляя в ячейке
    /// побитовое И нового значения со старым. Пишет прямо в раздел, а не через
    /// `write_firmware` из `embassy-boot`: тот стирал бы сектор при каждой
    /// записи, потому что апдейтер здесь создаётся заново на каждый вызов.
    ///
    /// # Errors
    ///
    /// `BadState` — раздел занят; `Flash` — отказ записи.
    pub async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), UpdaterError> {
        // Запрет на запись, пока раздел занят, — та самая проверка, с которой
        // начинался `write_firmware` (`verify_booted`), только строже на одно
        // состояние. Прямая запись её не делает, а без неё приложение затёрло
        // бы образ, в который bootloader собирается откатиться.
        if self.is_busy().await? {
            return Err(UpdaterError::BadState);
        }
        self.parts
            .dfu
            .write(offset, data)
            .await
            .map_err(|error| UpdaterError::Flash(error.kind()))
    }

    /// Читает записанное обратно — проверить принятое, не полагаясь на то, что
    /// запись прошла успешно. Разрешено и когда раздел занят: чтение ничего не
    /// портит.
    ///
    /// # Errors
    ///
    /// Отказ чтения.
    pub async fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), UpdaterError> {
        Ok(self.updater().read_dfu(offset, buf).await?)
    }

    /// Подтверждает работоспособность текущего образа и отменяет откат.
    ///
    /// Без этого вызова обновление живёт ровно один запуск: следующий сброс
    /// вернёт предыдущий образ.
    ///
    /// # Errors
    ///
    /// Отказ флеша при пометке.
    pub async fn mark_booted(&mut self) -> Result<(), UpdaterError> {
        Ok(self.updater().mark_booted().await?)
    }

    /// Просит bootloader поменять разделы местами на следующем сбросе. Сам
    /// сброс — за вызывающим.
    ///
    /// Проверок здесь нет намеренно: они — в `ota::apply`, и обмен
    /// запрашивается только через неё. Она сверяет длину, раскладку и
    /// версию, оставляет bootloader'у запись о подписи ([`PendingImage`]) и
    /// только потом зовёт этот метод. Прямой вызов мимо неё бессмыслен: обмен
    /// без записи bootloader отвергнет (`ScreenFailure::NoRecord`).
    ///
    /// Пометка — через `FirmwareState`, а не апдейтер: у апдейтера
    /// `mark_updated` есть только без фичи `_verify` у `embassy-boot`
    /// (`#[cfg(not(feature = "_verify"))]`), у состояния — всегда, и делает
    /// оно ровно то же (`set_magic(SWAP_MAGIC)`).
    ///
    /// # Errors
    ///
    /// Отказ флеша при пометке.
    pub async fn mark_updated(&mut self) -> Result<(), UpdaterError> {
        Ok(self.state().mark_updated().await?)
    }

    /// Апдейтер держит ссылки и на разделы, и на буфер, поэтому хранить его
    /// полем нельзя — структура вышла бы самоссылающейся. Собирается он
    /// дёшево (две ссылки и срез), так что каждая операция делает себе свой.
    fn updater(&mut self) -> FirmwareUpdater<'_, &mut D, &mut S> {
        let config = FirmwareUpdaterConfig {
            dfu: &mut self.parts.dfu,
            state: &mut self.parts.state,
        };
        FirmwareUpdater::new(config, &mut self.aligned.0[..S::WRITE_SIZE])
    }

    /// Состояние bootloader'а над разделом `STATE` (см. `mark_updated`).
    fn state(&mut self) -> FirmwareState<'_, &mut S> {
        FirmwareState::new(&mut self.parts.state, &mut self.aligned.0[..S::WRITE_SIZE])
    }
}

/// Разделы — тестам, которые смотрят во флеш под адаптером и разыгрывают его
/// отказы (`ota::fake`).
#[cfg(test)]
impl<P> Updater<P> {
    pub(crate) fn partitions(&mut self) -> &mut P {
        &mut self.parts
    }
}

#[cfg(test)]
mod tests {
    use adapters_hw::flash::fake::MemFlash;
    use embedded_storage::nor_flash::NorFlashErrorKind;
    use ports::Partitions;

    use super::{Updater, UpdaterError};

    /// Отпечаток раскладки в тестах — адаптер отдаёт его как получил.
    const LAYOUT: u32 = 0x5EED_1A70;

    /// Четыре страницы по 256 байт, слово 8; ACTIVE (= capacity) — три
    /// страницы, как требует запас на обмен.
    type Dfu = MemFlash<1024, 8, 256>;
    /// Раздел состояния: страница на магию плюс журнал прогресса.
    type State = MemFlash<256, 8, 256>;

    /// Адаптер над разделами в памяти.
    type Mem = Updater<Partitions<Dfu, State>>;

    /// Адаптер над `dfu` и `state` с самым длинным образом `capacity`.
    fn mem(dfu: Dfu, state: State, capacity: u32) -> Mem {
        Updater::new(Partitions {
            dfu,
            state,
            capacity,
            layout: LAYOUT,
        })
    }

    fn updater() -> Mem {
        mem(Dfu::erased(), State::erased(), 768)
    }

    fn with_state(magic: u8) -> Mem {
        let mut state = State::erased();
        state.mem[..8].fill(magic);
        mem(Dfu::erased(), state, 768)
    }

    /// Сквозной путь: `Updater` готовит раздел, образ и запись ложатся туда,
    /// куда пишет `ota::apply` (смещение `capacity`), — и bootloader
    /// находит их там же.
    #[cfg(feature = "boot")]
    #[tokio::test]
    async fn what_the_updater_writes_is_what_the_bootloader_reads() {
        use crate::ota::boot::{Screening, screen_update};
        use ports::PendingImage;

        const SEED: [u8; 32] = [42; 32];
        let image: Vec<u8> = (0..600u32).map(|i| (i % 251) as u8).collect();
        let len = u32::try_from(image.len()).expect("образ короткий");
        let digest = salty::Sha512::new().updated(&image).finalize();
        let keys = salty::Keypair::from(&SEED);
        let record = PendingImage {
            len,
            signature: keys.sign(&digest).to_bytes(),
        };
        let mut u = mem(Dfu::filled(0x00), State::erased(), 768);
        u.prepare(len).await.expect("подготовка");
        let mut padded = image.clone();
        padded.resize(image.len().next_multiple_of(8), 0xFF);
        u.write(0, &padded).await.expect("образ");
        u.write(768, &record.to_bytes()).await.expect("запись");
        u.mark_updated().await.expect("запросить обмен");

        let screening = screen_update(
            &mut u.parts.dfu,
            &mut u.parts.state,
            768,
            &keys.public.to_bytes(),
        );

        assert_eq!(screening.expect("проверка"), Screening::Approved);
    }

    /// Геометрия, при которой запись для bootloader'а легла бы мимо раздела,
    /// отвергается при сборке адаптера.
    #[test]
    #[should_panic(expected = "не кратен странице")]
    fn refuses_a_capacity_off_the_erase_page() {
        let _ = mem(Dfu::erased(), State::erased(), 700);
    }

    #[test]
    #[should_panic(expected = "запасная страница")]
    fn refuses_a_dfu_without_room_for_the_spare_page() {
        // Вместимость DFU равна `capacity`: за образом страницы под запись нет.
        let _ = mem(Dfu::erased(), State::erased(), 1024);
    }

    /// `prepare` уступает исполнитель после каждой страницы: три страницы
    /// `ACTIVE` и запасная — четыре `Pending`, и только пятый `poll`
    /// завершает подготовку.
    #[test]
    fn prepare_yields_after_each_page() {
        use core::pin::pin;
        use core::task::{Context, Waker};

        let mut u = mem(Dfu::filled(0x00), State::erased(), 768);
        let mut cx = Context::from_waker(Waker::noop());
        let mut prepare = pin!(u.prepare(512));

        let pending = (0..8)
            .take_while(|_| prepare.as_mut().poll(&mut cx).is_pending())
            .count();

        assert_eq!(pending, 4, "по уступке на каждую из четырёх страниц");
    }

    /// Короткий образ — а стёрт весь `DFU`: и страницы за образом, которые
    /// обмен всё равно перенесёт в `ACTIVE`, и запасная под запись для
    /// bootloader'а. Остатки прошлой загрузки за образом подпись не покрывает.
    #[tokio::test]
    async fn prepare_erases_everything_the_swap_moves_and_the_spare_page() {
        let mut u = mem(Dfu::filled(0x00), State::erased(), 768);

        u.prepare(8).await.expect("стереть раздел");

        assert!(
            u.parts.dfu.mem.iter().all(|b| *b == 0xFF),
            "весь DFU стёрт, а не только страница под образ"
        );
    }

    #[tokio::test]
    async fn prepare_refuses_zero_and_oversized_lengths_before_erasing() {
        for len in [0, 769] {
            let mut u = mem(Dfu::filled(0x00), State::erased(), 768);

            let refused = u.prepare(len).await;

            assert!(
                refused == Err(UpdaterError::Flash(NorFlashErrorKind::OutOfBounds)),
                "{len}"
            );
            assert!(
                u.parts.dfu.mem.iter().all(|b| *b == 0x00),
                "раздел не должен быть стёрт"
            );
        }
    }

    #[tokio::test]
    async fn write_and_prepare_refuse_while_a_swap_is_pending_or_just_reverted() {
        // 0xF0 — SWAP_MAGIC, 0xC0 — REVERT_MAGIC (`embassy-boot`, lib.rs).
        for magic in [0xF0, 0xC0] {
            let mut u = with_state(magic);

            assert!(u.is_busy().await.expect("состояние"), "{magic:#x}");
            assert_eq!(u.prepare(8).await, Err(UpdaterError::BadState));
            assert_eq!(u.write(0, &[0; 8]).await, Err(UpdaterError::BadState));
            assert!(
                u.read(0, &mut [0; 8]).await.is_ok(),
                "чтение занятого раздела разрешено"
            );
        }
    }

    #[tokio::test]
    async fn a_booted_state_is_not_busy() {
        // 0xD0 — BOOT_MAGIC; стёртый раздел читается как Boot тоже.
        assert!(!with_state(0xD0).is_busy().await.expect("состояние"));
        assert!(!updater().is_busy().await.expect("состояние"));
    }

    #[tokio::test]
    async fn writes_and_reads_back_after_prepare() {
        let mut u = updater();
        u.prepare(16).await.expect("подготовка");

        u.write(0, &[0xA5; 8]).await.expect("первая запись");
        u.write(8, &[0x5A; 8])
            .await
            .expect("вторая запись в тот же сектор");
        let mut back = [0; 16];
        u.read(0, &mut back).await.expect("чтение");

        assert_eq!(&back[..8], &[0xA5; 8]);
        assert_eq!(&back[8..], &[0x5A; 8]);
    }

    #[tokio::test]
    async fn mark_updated_makes_the_partition_busy_and_mark_booted_frees_it() {
        let mut u = updater();

        u.mark_updated().await.expect("пометить обмен");
        assert!(u.is_busy().await.expect("состояние"));

        u.mark_booted().await.expect("подтвердить");
        assert!(!u.is_busy().await.expect("состояние"));
    }

    #[test]
    fn reports_the_flash_geometry() {
        let u = updater();
        assert_eq!(u.write_granularity(), 8);
        assert_eq!(u.erase_size(), 256);
        assert_eq!(u.capacity(), 768);
    }

    /// Отпечаток раскладки доходит от нарезки (`Partitions`) до логики
    /// без подмены: на МК его считает плата (`bsp::layout`), и адаптер не вправе
    /// отдавать вместо него что-то своё.
    #[test]
    fn reports_the_layout_from_its_partitions() {
        let u: Mem = Updater::new(Partitions {
            dfu: Dfu::erased(),
            state: State::erased(),
            capacity: 768,
            layout: 0x1234_5678,
        });
        assert_eq!(u.layout(), 0x1234_5678);
    }
}
