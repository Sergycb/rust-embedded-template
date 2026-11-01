//! Обновление прошивки поверх `embassy-boot`: разделы `DFU` (куда пишется
//! новый образ) и `BOOTLOADER_STATE` (где лежит решение bootloader'а) — два
//! `NorFlash`, откуда они взялись, адаптеру всё равно. Режет их плата
//! (`bsp::flash`) по символам `memory.x` и отдаёт готовыми — [`Partitions`](crate::ota::Partitions);
//! в host-тесте это разделы в памяти.
//!
//! Что здесь есть: подготовка раздела под образ, запись и чтение кусков,
//! состояние bootloader'а, подтверждение образа, пометка обмена — это
//! [`Updater`](crate::ota::Updater), реализация порта `ports::FirmwareUpdate` для приложения, над
//! **асинхронными** разделами: за ними может стоять и внутренний флеш, и
//! внешняя микросхема на SPI. Чего нет: ни решения bootloader'а и проверки
//! подписи нового образа (`domain::ota::boot`, над блокирующими разделами:
//! у bootloader'а нет исполнителя), ни порядка проверок при применении
//! (`domain::ota::update`), ни буферизации до слова (`domain::ota::download`) —
//! это логика, а здесь только доступ к разделам.
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

use embassy_boot::{AlignedBuffer, FirmwareState, FirmwareUpdater, FirmwareUpdaterConfig, State};
use embedded_storage::nor_flash::NorFlashErrorKind;
use embedded_storage_async::nor_flash::NorFlash;
use ports::FirmwareUpdate;
// Тип возвращается портом, поэтому называть его должно быть чем — иначе
// пользователю пришлось бы объявлять прямую зависимость на `embassy-boot`
// только ради имени в сигнатуре своей функции.
use embassy_boot::FirmwareUpdaterError as Error;

pub use ports::MAX_WRITE_SIZE;

/// Разделы обновления — то, над чем работает [`Updater`]: раздел `DFU`, куда
/// принимается образ, журнал обмена `BOOTLOADER_STATE`, самый длинный образ и
/// отпечаток раскладки.
///
/// Нарезает их плата по символам `memory.x` (`bsp::flash::firmware`) и
/// отдаёт полем `Board`; [`Updater`] над ними строит compose-site узла OTA.
#[derive(Debug)]
pub struct Partitions<D, S> {
    /// Раздел `DFU`.
    pub dfu: D,
    /// Раздел `BOOTLOADER_STATE`.
    pub state: S,
    /// Самый длинный принимаемый образ — размер `ACTIVE`, а не `DFU`
    /// (см. `FirmwareUpdate::capacity`).
    pub capacity: u32,
    /// Отпечаток раскладки, под которую собрана эта прошивка
    /// (`ports::layout_fingerprint`, см. `FirmwareUpdate::layout`).
    pub layout: u32,
}

/// Доступ к разделам обновления `P` — [`Partitions`] платы.
///
/// `prepare` стирает всё, что обмен перенесёт в `ACTIVE` (`DFU[..capacity]`),
/// и запасную страницу за ним — туда `domain::ota::update::apply` кладёт
/// запись для bootloader'а (`ports::PendingImage`); оставь её нестёртой —
/// запись легла бы поверх остатков прошлого обмена.
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
            assert!(
                ports::PendingImage::LEN.is_multiple_of(D::WRITE_SIZE),
                "запись для bootloader'а не кратна слову записи DFU"
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
        u32::try_from(D::ERASE_SIZE).expect("ERASE_SIZE не должен превышать u32::MAX")
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

    /// Пометка обмена — через `FirmwareState`, а не апдейтер: тот прячет
    /// `mark_updated` под фичей `_verify` у `embassy-boot`, а состояние — нет,
    /// и делает ровно то же (`set_magic(SWAP_MAGIC)`). Подпись проверяет
    /// bootloader (`screen_update`): без записи о подписи он обмен не
    /// пропустит.
    fn state(&mut self) -> FirmwareState<'_, &mut S> {
        FirmwareState::new(&mut self.parts.state, &mut self.aligned.0[..S::WRITE_SIZE])
    }

    /// Занят ли раздел `DFU` тем, что терять нельзя — см.
    /// [`FirmwareUpdate::is_busy`]. `embassy-boot` в `verify_booted` `Revert`
    /// разрешает, и это его право: он отвечает на «можно ли писать вообще», а
    /// не на «не жалко ли того, что лежит в разделе». Здесь правило строже
    /// намеренно: README советует переносить `mark_booted()` туда, где
    /// устройство доказало работоспособность, то есть окно `Revert` в реальном
    /// проекте живёт долго, и начатая в нём передача стирала бы образ отката.
    async fn dfu_is_busy(&mut self) -> Result<bool, Error> {
        Ok(matches!(
            self.updater().get_state().await?,
            State::Swap | State::Revert
        ))
    }
}

/// Порт домена. Собственных методов с теми же именами у `Updater` нет
/// намеренно: они перекрыли бы трейтовые при вызове, и приложение работало бы
/// с конкретным типом мимо порта — то есть мимо той границы, ради которой
/// порт заведён.
impl<D: NorFlash, S: NorFlash> FirmwareUpdate for Updater<Partitions<D, S>> {
    type Error = Error;

    fn write_granularity(&mut self) -> u32 {
        u32::try_from(D::WRITE_SIZE).expect("WRITE_SIZE не должен превышать u32::MAX")
    }

    fn capacity(&mut self) -> Result<u32, Self::Error> {
        Ok(self.parts.capacity)
    }

    fn layout(&self) -> u32 {
        self.parts.layout
    }

    async fn is_busy(&mut self) -> Result<bool, Self::Error> {
        self.dfu_is_busy().await
    }

    /// Стирает `DFU[..capacity]` — всё, что обмен `embassy-boot` перенесёт в
    /// `ACTIVE`, а не только страницы под образ длиной `len`, — и запасную
    /// страницу под запись для bootloader'а. Подпись покрывает только `len`
    /// байт, а обмен копирует раздел целиком: нестёртый хвост с остатками
    /// прошлых загрузок (в том числе чужого, отвергнутого bootloader'ом
    /// образа) уехал бы в `ACTIVE` непроверенным. Худшее время от этого не
    /// растёт — образ во весь `ACTIVE` стирал столько же; дольше становится
    /// только приём короткого образа (docs/ota.md, «Приём образа»).
    /// `len` нужна для отказа до стирания.
    ///
    /// Стирает по странице и уступает исполнитель после каждой: флеш платы
    /// блокирующий (`BlockingAsync`), и стирание всего диапазона одним вызовом
    /// держало бы все задачи прошивки, пока идёт стирание. Короче одной страницы
    /// пауза не станет — её стирание неделимо (сектор F4 — 128 KiB, до 2 с).
    async fn prepare(&mut self, len: u32) -> Result<(), Self::Error> {
        if self.dfu_is_busy().await? {
            return Err(Error::BadState);
        }
        // Негодная длина — отказ ДО стирания: стерев раздел, мы уничтожаем
        // образ, в который bootloader откатывается. Ноль отвергается по той же
        // причине: стирать нечего, а следующая запись легла бы в нестёртую
        // память.
        if len == 0 || len > self.parts.capacity {
            return Err(Error::Flash(NorFlashErrorKind::OutOfBounds));
        }
        // `capacity` кратна странице, и за ней помещается запасная — это
        // проверил `new`, поэтому `end` не выходит за раздел.
        let page = self.erase_size();
        let end = self.parts.capacity + page;
        for from in (0..end).step_by(page as usize) {
            self.parts.dfu.erase(from, from + page).await?;
            embassy_futures::yield_now().await;
        }
        Ok(())
    }

    /// Пишет кусок прямо в раздел, а не через `write_firmware` из
    /// `embassy-boot`: тот стирал бы сектор при каждой записи, потому что
    /// апдейтер здесь создаётся заново на каждый вызов (docs/ota.md, «Приём
    /// образа: стирание отдельно от записи»).
    async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        // Запрет на запись, пока раздел занят, — та самая проверка, с которой
        // начинался `write_firmware` (`verify_booted`), только строже на одно
        // состояние. Прямая запись её не делает, а без неё приложение затёрло
        // бы образ, в который bootloader собирается откатиться.
        if self.dfu_is_busy().await? {
            return Err(Error::BadState);
        }
        self.parts.dfu.write(offset, data).await?;
        Ok(())
    }

    async fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.updater().read_dfu(offset, buf).await
    }

    async fn mark_booted(&mut self) -> Result<(), Self::Error> {
        self.updater().mark_booted().await
    }

    async fn mark_updated(&mut self) -> Result<(), Self::Error> {
        self.state().mark_updated().await
    }
}

#[cfg(test)]
mod tests {
    use embedded_storage::nor_flash::NorFlashErrorKind;
    use ports::FirmwareUpdate;

    use super::{Error, Updater};
    use crate::ota::fakes::{Dfu, State};

    /// Отпечаток раскладки в тестах — адаптер отдаёт его как получил.
    const LAYOUT: u32 = 0x5EED_1A70;

    /// Адаптер над разделами в памяти.
    type Mem = Updater<super::Partitions<Dfu, State>>;

    /// Адаптер над `dfu` и `state` с самым длинным образом `capacity`.
    fn mem(dfu: Dfu, state: State, capacity: u32) -> Mem {
        Updater::new(super::Partitions {
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

    /// Сквозной путь через порт: `Updater` готовит раздел, образ и
    /// запись ложатся туда, куда пишет `domain::ota::update::apply`
    /// (смещение `capacity`), — и bootloader находит их там же.
    #[tokio::test]
    async fn what_the_signed_port_writes_is_what_the_bootloader_reads() {
        use domain::ota::boot::{Screening, screen_update};
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
                matches!(refused, Err(Error::Flash(NorFlashErrorKind::OutOfBounds))),
                "{len}"
            );
            assert!(
                u.parts.dfu.mem.iter().all(|b| *b == 0x00),
                "раздел не должен быть стёрт"
            );
        }
    }

    #[tokio::test]
    async fn write_and_prepare_refuse_while_the_partition_holds_a_rollback_image() {
        // 0xF0 — SWAP_MAGIC, 0xC0 — REVERT_MAGIC (`embassy-boot`, lib.rs).
        for magic in [0xF0, 0xC0] {
            let mut u = with_state(magic);

            assert!(u.is_busy().await.expect("состояние"), "{magic:#x}");
            assert!(matches!(u.prepare(8).await, Err(Error::BadState)));
            assert!(matches!(u.write(0, &[0; 8]).await, Err(Error::BadState)));
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
        let mut u = updater();
        assert_eq!(u.write_granularity(), 8);
        assert_eq!(u.erase_size(), 256);
        assert_eq!(u.capacity().expect("вместимость"), 768);
    }

    /// Отпечаток раскладки доходит от нарезки (`Partitions`) до порта
    /// без подмены: на МК его считает плата (`bsp::layout`), и адаптер не вправе
    /// отдавать вместо него что-то своё.
    #[test]
    fn reports_the_layout_from_its_partitions() {
        let u: Mem = Updater::new(super::Partitions {
            dfu: Dfu::erased(),
            state: State::erased(),
            capacity: 768,
            layout: 0x1234_5678,
        });
        assert_eq!(u.layout(), 0x1234_5678);
    }
}

#[cfg(test)]
mod fakes {
    //! Флеш в памяти для тестов `Updater`.

    use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};

    /// Флеш в памяти с семантикой NOR: стирание страницами по 256 байт в
    /// `0xFF`, запись словами по 8 байт и только «единицы в нули». Готового
    /// мока `NorFlash` в экосистеме нет (`embedded-hal-mock` флеш не
    /// покрывает), поэтому фейк свой и ровно под тесты `ota`.
    #[derive(Debug)]
    pub(super) struct MemFlash<const SIZE: usize> {
        pub(super) mem: Vec<u8>,
    }

    impl<const SIZE: usize> MemFlash<SIZE> {
        pub(super) fn erased() -> Self {
            Self::filled(0xFF)
        }

        pub(super) fn filled(byte: u8) -> Self {
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

    // `Updater` работает с асинхронными разделами, а сквозной тест передаёт те
    // же разделы проверке bootloader'а (`domain::ota::boot`) — блокирующей:
    // фейк умеет оба, асинхронные — поверх блокирующих (готовы с первого
    // `poll`).
    impl<const SIZE: usize> embedded_storage_async::nor_flash::ReadNorFlash for MemFlash<SIZE> {
        const READ_SIZE: usize = 1;

        fn read(
            &mut self,
            offset: u32,
            bytes: &mut [u8],
        ) -> impl Future<Output = Result<(), Self::Error>> {
            core::future::ready(ReadNorFlash::read(self, offset, bytes))
        }

        fn capacity(&self) -> usize {
            SIZE
        }
    }

    impl<const SIZE: usize> embedded_storage_async::nor_flash::NorFlash for MemFlash<SIZE> {
        const WRITE_SIZE: usize = 8;
        const ERASE_SIZE: usize = 256;

        fn erase(&mut self, from: u32, to: u32) -> impl Future<Output = Result<(), Self::Error>> {
            core::future::ready(NorFlash::erase(self, from, to))
        }

        fn write(
            &mut self,
            offset: u32,
            bytes: &[u8],
        ) -> impl Future<Output = Result<(), Self::Error>> {
            core::future::ready(NorFlash::write(self, offset, bytes))
        }
    }

    /// Четыре страницы по 256 байт, слово 8; ACTIVE (= capacity) — три
    /// страницы, как требует запас на обмен.
    pub(super) type Dfu = MemFlash<1024>;
    /// Раздел состояния: страница на магию плюс журнал прогресса.
    pub(super) type State = MemFlash<256>;
}
