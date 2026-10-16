//! Обновление прошивки поверх `embassy-boot`: разделы `DFU` (куда пишется
//! новый образ) и `BOOTLOADER_STATE` (где лежит решение bootloader'а) — два
//! `NorFlash`, откуда они взялись, адаптеру всё равно. Прошивка строит их из
//! линкерных символов (`crates-cross/bsp/src/ota.rs`), host-тест — из памяти.
//!
//! Что здесь есть: подготовка раздела под образ, запись и чтение кусков,
//! состояние bootloader'а, подтверждение образа, пометка обмена — это
//! [`Updater`](crate::ota::Updater), реализация порта `ports::FirmwareUpdate` для приложения, над
//! **асинхронными** разделами: за ними может стоять и внутренний флеш, и
//! внешняя микросхема на SPI. И — под фичей `signed` — проверка подписи
//! нового образа, которую зовёт bootloader (`screen_update`, над
//! блокирующими разделами: bootloader без исполнителя). Чего нет: ни порядка
//! проверок при применении (`domain::ota::update`), ни буферизации до слова
//! (`domain::ota::download`) — это логика, а здесь только доступ к разделам.
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

mod boot;

use embassy_boot::{AlignedBuffer, FirmwareState, FirmwareUpdater, FirmwareUpdaterConfig, State};
use embedded_storage::nor_flash::NorFlashErrorKind;
use embedded_storage_async::nor_flash::NorFlash;
use ports::FirmwareUpdate;
// Тип возвращается портом, поэтому называть его должно быть чем — иначе
// пользователю пришлось бы объявлять прямую зависимость на `embassy-boot`
// только ради имени в сигнатуре своей функции.
pub use embassy_boot::FirmwareUpdaterError as Error;

pub use boot::{BootPlan, boot_plan};
#[cfg(feature = "signed")]
pub use boot::{Rejection, Screening, screen_update};

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
    /// Проект с подписью: `prepare` стирает и запасную страницу за `capacity`
    /// — туда ляжет запись для bootloader'а (`ports::PendingImage`).
    signed: bool,
}

impl<DFU: NorFlash, STATE: NorFlash> core::fmt::Debug for Updater<DFU, STATE> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Updater").finish_non_exhaustive()
    }
}

impl<DFU: NorFlash, STATE: NorFlash> Updater<DFU, STATE> {
    /// Проект без подписи. `capacity` — размер раздела, в который образ будет
    /// исполняться (`ACTIVE`), см. поле.
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
            signed: false,
        }
    }

    /// Проект с подписью: то же, но `prepare` стирает и запасную страницу
    /// `DFU` за `capacity`. Туда `domain::ota::update::apply_signed` кладёт
    /// запись для bootloader'а; обычная подготовка её не трогает, а оставь её
    /// нестёртой — запись легла бы поверх остатков прошлого обмена. Цена —
    /// ещё одно стирание страницы на обновление (на F4 — сектор 128 KiB).
    pub fn signed(dfu: DFU, state: STATE, capacity: u32) -> Self {
        const {
            assert!(
                ports::PendingImage::LEN.is_multiple_of(DFU::WRITE_SIZE),
                "запись для bootloader'а не кратна слову записи DFU"
            );
        }
        Self {
            signed: true,
            ..Self::new(dfu, state, capacity)
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
    fn updater(&mut self) -> FirmwareUpdater<'_, &mut DFU, &mut STATE> {
        let config = FirmwareUpdaterConfig {
            dfu: &mut self.dfu,
            state: &mut self.state,
        };
        FirmwareUpdater::new(config, &mut self.aligned.0[..STATE::WRITE_SIZE])
    }

    /// Пометка обмена — через `FirmwareState`, а не апдейтер: тот прячет
    /// `mark_updated` под фичей `_verify` у `embassy-boot`, а состояние — нет,
    /// и делает ровно то же (`set_magic(SWAP_MAGIC)`). Защита от
    /// неподписанного образа в проекте с подписью — bootloader
    /// (`screen_update`): без записи о подписи он обмен не пропустит.
    fn state(&mut self) -> FirmwareState<'_, &mut STATE> {
        FirmwareState::new(&mut self.state, &mut self.aligned.0[..STATE::WRITE_SIZE])
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
impl<DFU: NorFlash, STATE: NorFlash> FirmwareUpdate for Updater<DFU, STATE> {
    type Error = Error;

    fn write_granularity(&mut self) -> u32 {
        u32::try_from(DFU::WRITE_SIZE).expect("WRITE_SIZE не должен превышать u32::MAX")
    }

    fn capacity(&mut self) -> Result<u32, Self::Error> {
        Ok(self.capacity)
    }

    async fn is_busy(&mut self) -> Result<bool, Self::Error> {
        self.dfu_is_busy().await
    }

    /// Стирает столько страниц раздела `DFU`, сколько нужно образу длиной
    /// `len` (и запасную, если проект с подписью). `prepare_update()` из
    /// `embassy-boot` здесь не годится: он стирает раздел целиком, а это и
    /// время (на крупных секторах — секунды), и лишний износ флеша.
    async fn prepare(&mut self, len: u32) -> Result<(), Self::Error> {
        if self.dfu_is_busy().await? {
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
        let page = self.erase_size();
        let end = len.div_ceil(page).saturating_mul(page);
        self.dfu.erase(0, end).await?;
        if self.signed {
            let spare = self.capacity;
            self.dfu.erase(spare, spare + page).await?;
        }
        Ok(())
    }

    /// Пишет кусок прямо в раздел, а не через `write_firmware` из
    /// `embassy-boot`: тот стирает сектор, «если он не тот, что стирали в
    /// прошлый раз», и помнит это полем апдейтера — объекта, который здесь
    /// создаётся заново на каждый вызов. У свежего «в прошлый раз» пусто, и
    /// каждая запись стирала бы сектор, в который пишет (проверено на живой
    /// плате: две записи в один сектор, первая читалась как `0xFF`).
    async fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        // Запрет на запись, пока раздел занят, — та самая проверка, с которой
        // начинался `write_firmware` (`verify_booted`), только строже на одно
        // состояние. Прямая запись её не делает, а без неё приложение затёрло
        // бы образ, в который bootloader собирается откатиться.
        if self.dfu_is_busy().await? {
            return Err(Error::BadState);
        }
        self.dfu.write(offset, data).await?;
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

    fn updater() -> Updater<Dfu, State> {
        Updater::new(Dfu::erased(), State::erased(), 768)
    }

    fn with_state(magic: u8) -> Updater<Dfu, State> {
        let mut state = State::erased();
        state.mem[..8].fill(magic);
        Updater::new(Dfu::erased(), state, 768)
    }

    #[tokio::test]
    async fn prepare_erases_only_the_pages_the_image_needs() {
        let mut u = Updater::new(Dfu::filled(0x00), State::erased(), 768);

        u.prepare(257).await.expect("стереть под образ");

        assert!(
            u.dfu.mem[..512].iter().all(|b| *b == 0xFF),
            "две страницы стёрты"
        );
        assert!(
            u.dfu.mem[512..].iter().all(|b| *b == 0x00),
            "третья и запасная не тронуты"
        );
    }

    /// С подписью `prepare` стирает и запасную страницу — под запись для
    /// bootloader'а; остальное — как без подписи.
    #[tokio::test]
    async fn signed_prepare_also_erases_the_spare_page() {
        let mut u = Updater::signed(Dfu::filled(0x00), State::erased(), 768);

        u.prepare(257).await.expect("стереть под образ и запись");

        assert!(u.dfu.mem[..512].iter().all(|b| *b == 0xFF));
        assert!(
            u.dfu.mem[512..768].iter().all(|b| *b == 0x00),
            "третья не тронута"
        );
        assert!(
            u.dfu.mem[768..].iter().all(|b| *b == 0xFF),
            "запасная стёрта"
        );
    }

    #[tokio::test]
    async fn prepare_refuses_zero_and_oversized_lengths_before_erasing() {
        for len in [0, 769] {
            let mut u = Updater::new(Dfu::filled(0x00), State::erased(), 768);

            let refused = u.prepare(len).await;

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
}

#[cfg(test)]
mod fakes {
    //! Флеш в памяти — общий для тестов `Updater` и решения bootloader'а.

    use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};

    /// Флеш в памяти с семантикой NOR: стирание страницами по 256 байт в
    /// `0xFF`, запись словами по 8 байт и только «единицы в нули». Готового
    /// мока `NorFlash` в экосистеме нет (`embedded-hal-mock` флеш не
    /// покрывает), поэтому фейк свой и ровно под тесты `ota` и `ota::boot`.
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

    // `Updater` работает с асинхронными разделами, `screen_update` и обмен
    // `embassy-boot` — с блокирующими: фейк умеет оба, асинхронные — поверх
    // блокирующих (готовы с первого `poll`).
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
