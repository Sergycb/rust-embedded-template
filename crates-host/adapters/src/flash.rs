//! Асинхронные трейты флеша поверх блокирующего `Flash`, живущего под
//! `blocking_mutex`.
//!
//! Нужен, потому что API `sequential-storage` 8.x асинхронный, а `Flash`
//! embassy — блокирующий и делится с OTA (`bsp::FlashMutex`). Взять `&mut F`
//! из мьютекса и держать его через `.await` нельзя (`lock` даёт доступ только
//! внутри замыкания), поэтому блокировка берётся на каждую операцию отдельно.
//! Это честно: сами операции блокирующие, ни одна из них внутри не ждёт, и
//! «асинхронность» здесь чистая формальность ради совместимости типов.
//!
//! Generic по мьютексу и флешу, а не привязан к `embassy_stm32::flash::Flash`:
//! мосту всё равно, что под ним, — а тестируется он на хосте с флешем в памяти.
//!
//! Блокирующие трейты [`Shared`](crate::flash::Shared) тоже реализует — ради второго мьютекса над тем
//! же флешем: `embassy-boot` строит каждый раздел из `&Mutex<_, RefCell<T>>`
//! своего типа, и раздел со своей страницей ([`Paged`](crate::flash::Paged)) получает свой мьютекс
//! над [`Shared`](crate::flash::Shared), а не второй объект `Flash`.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};

/// Флеш, к которому доступ идёт только под замком, по одной операции.
///
/// Трейт, а не `&Mutex<M, RefCell<F>>` в сигнатуре [`Shared`]: `RefCell`
/// нужен блокирующему `Mutex` embassy (его API другого не принимает), но это
/// деталь реализации, которая не должна расползаться по публичным типам.
/// Для самого `Mutex<M, RefCell<F>>` трейт реализован ниже; владелец общего
/// флеша может реализовать его на своём типе и спрятать ячейку.
pub trait FlashLock {
    /// Флеш под замком.
    type Flash: NorFlash;

    /// Выполняет `op` с исключительным доступом к флешу.
    fn with<R>(&self, op: impl FnOnce(&mut Self::Flash) -> R) -> R;
}

impl<M: RawMutex, F: NorFlash> FlashLock for Mutex<M, RefCell<F>> {
    type Flash = F;

    fn with<R>(&self, op: impl FnOnce(&mut F) -> R) -> R {
        self.lock(|flash| op(&mut flash.borrow_mut()))
    }
}

/// Ссылка на общий флеш; операции — под его замком, по одной.
#[derive(Debug)]
pub struct Shared<'a, L: FlashLock> {
    flash: &'a L,
}

impl<'a, L: FlashLock> Shared<'a, L> {
    /// Оборачивает общий флеш.
    #[must_use]
    pub const fn new(flash: &'a L) -> Self {
        Self { flash }
    }
}

impl<L: FlashLock> ErrorType for Shared<'_, L> {
    type Error = <L::Flash as ErrorType>::Error;
}

impl<L: FlashLock> embedded_storage_async::nor_flash::ReadNorFlash for Shared<'_, L> {
    const READ_SIZE: usize = <L::Flash as ReadNorFlash>::READ_SIZE;

    fn read(
        &mut self,
        offset: u32,
        bytes: &mut [u8],
    ) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(self.flash.with(|flash| flash.read(offset, bytes)))
    }

    fn capacity(&self) -> usize {
        self.flash.with(|flash| flash.capacity())
    }
}

impl<L: FlashLock> embedded_storage_async::nor_flash::NorFlash for Shared<'_, L> {
    const WRITE_SIZE: usize = <L::Flash as NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <L::Flash as NorFlash>::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(self.flash.with(|flash| flash.erase(from, to)))
    }

    fn write(
        &mut self,
        offset: u32,
        bytes: &[u8],
    ) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(self.flash.with(|flash| flash.write(offset, bytes)))
    }
}

impl<L: FlashLock> ReadNorFlash for Shared<'_, L> {
    const READ_SIZE: usize = <L::Flash as ReadNorFlash>::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.flash.with(|flash| flash.read(offset, bytes))
    }

    fn capacity(&self) -> usize {
        self.flash.with(|flash| flash.capacity())
    }
}

impl<L: FlashLock> NorFlash for Shared<'_, L> {
    const WRITE_SIZE: usize = <L::Flash as NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <L::Flash as NorFlash>::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.flash.with(|flash| flash.erase(from, to))
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.flash.with(|flash| flash.write(offset, bytes))
    }
}

/// Тот же флеш, но со страницей стирания `ERASE_SIZE` вместо его собственной.
///
/// Цельный `embassy_stm32::flash::Flash` объявляет страницей самый крупный
/// сектор чипа, а стирает по настоящим: `erase(from, to)` проходит сектор за
/// сектором и отказывает, только если границы диапазона не совпали с
/// границами секторов. На F2/F4/F7 сектора неравные (16, 64 и 128 KiB у F4),
/// и раздел, лежащий в мелких секторах, со страницей всего чипа не строится:
/// `Partition::new` из `embassy-embedded-hal` паникует, если смещение раздела
/// не кратно `ERASE_SIZE`, а `sequential-storage` делит раздел на страницы
/// именно этого размера. Обёртка объявляет страницей сектор, в котором раздел
/// действительно лежит, — остальное передаётся как есть.
///
/// Страница может быть и крупнее физической: на чипах с мелкими страницами
/// (L0: 128 Б) обмен `embassy-boot` стоит вести страницами в 1–4 KiB, иначе
/// журнал `BOOTLOADER_STATE` (`2 + 4N` слов, `N` = `ACTIVE / страница`) не
/// влезает в раздел. Тогда `ERASE_SIZE` обязан делиться на физическую
/// страницу, а `ACTIVE`/`DFU` лежать на границах `ERASE_SIZE`
/// (`Partition::new` и `assert_partitions` проверяют это сами) — тесты
/// `coarse_swap` гоняют настоящий `BootLoader` над такой раскладкой.
///
/// Границы `erase(from, to)` обёртка не проверяет и не расширяет: диапазон
/// уходит флешу как есть, и его выравнивание по физическим страницам
/// проверяет он сам (ошибка, а не стирание лишнего). Что `ERASE_SIZE`
/// согласован с настоящими секторами под разделом, обёртка проверить не
/// может: это знает раскладка (`chip-data-gen`), откуда число и приезжает.
#[derive(Debug)]
pub struct Paged<F, const ERASE_SIZE: usize> {
    flash: F,
}

impl<F, const ERASE_SIZE: usize> Paged<F, ERASE_SIZE> {
    /// Оборачивает флеш.
    #[must_use]
    pub const fn new(flash: F) -> Self {
        Self { flash }
    }
}

/// Страница [`Paged`], проверенная при компиляции: ненулевая и кратная слову
/// записи. Одна функция на оба семейства трейтов — блокирующий путь
/// (`embassy-boot`) и асинхронный (`sequential-storage`) проверяют одно и то же.
const fn page_size(erase_size: usize, write_size: usize) -> usize {
    assert!(
        erase_size > 0 && erase_size.is_multiple_of(write_size),
        "страница должна быть ненулевой и кратной слову записи"
    );
    erase_size
}

impl<F: ErrorType, const ERASE_SIZE: usize> ErrorType for Paged<F, ERASE_SIZE> {
    type Error = F::Error;
}

impl<F: ReadNorFlash, const ERASE_SIZE: usize> ReadNorFlash for Paged<F, ERASE_SIZE> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.flash.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.flash.capacity()
    }
}

impl<F: NorFlash, const ERASE_SIZE: usize> NorFlash for Paged<F, ERASE_SIZE> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = page_size(ERASE_SIZE, F::WRITE_SIZE);

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.flash.erase(from, to)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.flash.write(offset, bytes)
    }
}

impl<F, const ERASE_SIZE: usize> embedded_storage_async::nor_flash::ReadNorFlash
    for Paged<F, ERASE_SIZE>
where
    F: embedded_storage_async::nor_flash::ReadNorFlash,
{
    const READ_SIZE: usize = F::READ_SIZE;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.flash.read(offset, bytes).await
    }

    fn capacity(&self) -> usize {
        self.flash.capacity()
    }
}

impl<F, const ERASE_SIZE: usize> embedded_storage_async::nor_flash::NorFlash
    for Paged<F, ERASE_SIZE>
where
    F: embedded_storage_async::nor_flash::NorFlash,
{
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = page_size(ERASE_SIZE, F::WRITE_SIZE);

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.flash.erase(from, to).await
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.flash.write(offset, bytes).await
    }
}

/// `ERASE_SIZE` асинхронного флеша — для проверок при компиляции там, где
/// трейт не в зависимостях (`bsp::external` проверяет им, что стирание
/// драйвера делит страницу обмена раскладки).
#[must_use]
pub const fn erase_size<F: embedded_storage_async::nor_flash::NorFlash>() -> usize {
    F::ERASE_SIZE
}

/// Внешняя флеш, которую ещё не подключили, — место под ваш драйвер.
///
/// Шаблон не знает, какая микросхема будет на плате (W25Q, GD25, MX25, IS25,
/// SST26…), и не гадает: хранилище настроек принимает любой асинхронный
/// `NorFlash`, а это — заглушка в его слоте, как `bsp::ota::Link` в слоте
/// канала OTA. Проект с ней собирается и запускается, а каждая операция
/// отвечает [`NotConnected`] — настройки не сохраняются, но и ничего не
/// падает. Заменить её — значит подставить в `bsp::config` свой драйвер:
/// готовый крейт (`spi-memory-async`, `w25q32jv`…) или собственный.
///
/// Геометрия правдоподобная — две страницы по 4 KiB, сектор почти любой
/// SPI NOR, — ровно столько, сколько нужно хранилищу, чтобы собраться.
///
/// Тот же слот и у OTA с разделами на внешней флеши (`ota=external`): в
/// приложении — асинхронные трейты, в bootloader'е — блокирующие, поэтому
/// заглушка реализует оба. Bootloader с ней не обменивает разделы, а
/// загружает текущий образ.
#[derive(Debug, Default)]
pub struct Unconnected;

impl Unconnected {
    /// Объём, под который строится хранилище с заглушкой.
    pub const CAPACITY: u32 = 2 * Self::PAGE;

    /// Страница стирания.
    const PAGE: u32 = 4096;
}

/// Ответ [`Unconnected`] на любую операцию.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct NotConnected;

impl embedded_storage::nor_flash::NorFlashError for NotConnected {
    fn kind(&self) -> embedded_storage::nor_flash::NorFlashErrorKind {
        embedded_storage::nor_flash::NorFlashErrorKind::Other
    }
}

impl ErrorType for Unconnected {
    type Error = NotConnected;
}

impl embedded_storage_async::nor_flash::ReadNorFlash for Unconnected {
    const READ_SIZE: usize = 1;

    fn read(&mut self, _: u32, _: &mut [u8]) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Err(NotConnected))
    }

    fn capacity(&self) -> usize {
        Self::CAPACITY as usize
    }
}

impl embedded_storage_async::nor_flash::NorFlash for Unconnected {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = Self::PAGE as usize;

    fn erase(&mut self, _: u32, _: u32) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Err(NotConnected))
    }

    fn write(&mut self, _: u32, _: &[u8]) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Err(NotConnected))
    }
}

impl ReadNorFlash for Unconnected {
    const READ_SIZE: usize = 1;

    fn read(&mut self, _: u32, _: &mut [u8]) -> Result<(), Self::Error> {
        Err(NotConnected)
    }

    fn capacity(&self) -> usize {
        Self::CAPACITY as usize
    }
}

impl NorFlash for Unconnected {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = Self::PAGE as usize;

    fn erase(&mut self, _: u32, _: u32) -> Result<(), Self::Error> {
        Err(NotConnected)
    }

    fn write(&mut self, _: u32, _: &[u8]) -> Result<(), Self::Error> {
        Err(NotConnected)
    }
}

#[cfg(test)]
mod tests {
    use core::cell::RefCell;

    use embassy_sync::blocking_mutex::Mutex;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use embedded_storage::nor_flash::{ErrorType, NorFlashErrorKind};
    use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};

    use super::{Paged, Shared, Unconnected};

    /// Блокирующий флеш в памяти: 64 байта, страница 16, слово 4. Мосту
    /// неважно, что под ним, — проверяется только, что каждая операция дошла
    /// до флеша и задела ровно свой диапазон.
    #[derive(Debug)]
    struct MemFlash {
        mem: [u8; 64],
    }

    impl ErrorType for MemFlash {
        type Error = NorFlashErrorKind;
    }

    impl embedded_storage::nor_flash::ReadNorFlash for MemFlash {
        const READ_SIZE: usize = 1;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            bytes.copy_from_slice(&self.mem[start..start + bytes.len()]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            self.mem.len()
        }
    }

    impl embedded_storage::nor_flash::NorFlash for MemFlash {
        const WRITE_SIZE: usize = 4;
        const ERASE_SIZE: usize = 16;

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            self.mem[from as usize..to as usize].fill(0xFF);
            Ok(())
        }

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            let start = offset as usize;
            self.mem[start..start + bytes.len()].copy_from_slice(bytes);
            Ok(())
        }
    }

    #[tokio::test]
    async fn forwards_every_operation_to_the_flash_under_the_mutex() {
        let flash: Mutex<NoopRawMutex, _> = Mutex::new(RefCell::new(MemFlash { mem: [0; 64] }));
        let mut shared = Shared::new(&flash);

        assert_eq!(shared.capacity(), 64);
        assert_eq!(
            <Shared<'_, Mutex<NoopRawMutex, RefCell<MemFlash>>> as NorFlash>::WRITE_SIZE,
            4
        );

        shared.erase(16, 32).await.expect("стирание");
        shared.write(16, &[0xA5; 4]).await.expect("запись");
        let mut back = [0; 8];
        shared.read(16, &mut back).await.expect("чтение");

        assert_eq!(back, [0xA5, 0xA5, 0xA5, 0xA5, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(
            flash.lock(|f| f.borrow().mem[..16].iter().all(|b| *b == 0)),
            "соседняя страница не тронута"
        );
    }

    /// С заглушкой хранилище собирается, а операции честно отказывают, а не
    /// паникуют: проект без подключённой флеши работает, просто без настроек.
    #[tokio::test]
    async fn settings_over_an_unconnected_flash_refuse_without_panicking() {
        use ports::SettingsStorage;

        let mut settings = crate::settings::Settings::new(Unconnected, 0..Unconnected::CAPACITY);

        assert!(settings.write(1, b"x").await.is_err());
        assert!(settings.read(1, &mut [0; 8]).await.is_err());
    }

    /// `Paged` меняет только объявленную страницу: операции доходят до флеша
    /// с теми же границами — и по блокирующим трейтам (разделы `embassy-boot`),
    /// и по асинхронным (`sequential-storage`).
    #[tokio::test]
    async fn paged_declares_its_own_page_and_forwards_the_rest() {
        type Flash = Mutex<NoopRawMutex, RefCell<MemFlash>>;
        type Page32<'a> = Paged<Shared<'a, Flash>, 32>;
        let flash: Flash = Mutex::new(RefCell::new(MemFlash { mem: [0; 64] }));

        assert_eq!(<Page32<'_> as NorFlash>::ERASE_SIZE, 32);
        assert_eq!(
            <Page32<'_> as embedded_storage::nor_flash::NorFlash>::ERASE_SIZE,
            32
        );
        assert_eq!(
            <Page32<'_> as embedded_storage::nor_flash::NorFlash>::WRITE_SIZE,
            4
        );

        let mut paged = Page32::new(Shared::new(&flash));
        embedded_storage::nor_flash::NorFlash::erase(&mut paged, 32, 64).expect("стирание");
        embedded_storage::nor_flash::NorFlash::write(&mut paged, 32, &[0xA5; 4]).expect("запись");
        paged.write(36, &[0x5A; 4]).await.expect("запись");
        let mut back = [0; 12];
        paged.read(32, &mut back).await.expect("чтение");

        assert_eq!(back[..8], [0xA5, 0xA5, 0xA5, 0xA5, 0x5A, 0x5A, 0x5A, 0x5A]);
        assert!(back[8..].iter().all(|b| *b == 0xFF));
        assert!(
            flash.lock(|f| f.borrow().mem[..32].iter().all(|b| *b == 0)),
            "стирание не вышло за свой диапазон"
        );
    }

    /// Обмен `embassy-boot` над страницей, более крупной физической (L0:
    /// физическая 128 Б, страница обмена 1 KiB), — настоящий `BootLoader` над
    /// разделами, как собирает их прошивка: `ACTIVE`/`DFU` — окна в `Paged<_, 1024>`,
    /// `STATE` — окно в `Paged<_, STATE_PAGE>`, всё над одним флешем.
    #[allow(
        clippy::cast_possible_truncation,
        reason = "фейк: размеры в тестах малы"
    )]
    mod coarse_swap {
        use core::cell::RefCell;

        use embassy_boot::{BlockingFirmwareState, BootError, BootLoader, BootLoaderConfig, State};
        use embassy_sync::blocking_mutex::Mutex;
        use embassy_sync::blocking_mutex::raw::NoopRawMutex;
        use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};

        use crate::flash::{Paged, Shared};

        /// Страница обмена и раскладка: ACTIVE — 3 страницы, DFU — 4 (запасная).
        const SWAP: usize = 1024;
        const ACTIVE_LEN: u32 = 3 * SWAP as u32;
        const DFU_OFFSET: u32 = ACTIVE_LEN;
        const DFU_LEN: u32 = 4 * SWAP as u32;
        const STATE_OFFSET: u32 = DFU_OFFSET + DFU_LEN;
        const WORD: usize = 4;
        /// `2 + 4N` слов журнала, `N` = страниц в ACTIVE.
        const STATE_MIN: u32 = ((2 + 4 * 3) * WORD) as u32;
        const TOTAL: usize = STATE_OFFSET as usize + 1024;

        /// NOR в памяти: стирание физическими страницами `PHYS`, запись словами
        /// по 4 «единицы в нули»; после `budget` операций (страница стирания
        /// или запись) отказывает — питание пропало посреди обмена, а
        /// стирание умирает между страницами, оставляя часть стёртой.
        struct Sim<const PHYS: usize> {
            mem: Vec<u8>,
            budget: usize,
        }

        impl<const PHYS: usize> Sim<PHYS> {
            fn tick(&mut self) -> Result<(), NorFlashErrorKind> {
                self.budget = self.budget.checked_sub(1).ok_or(NorFlashErrorKind::Other)?;
                Ok(())
            }
        }

        impl<const PHYS: usize> ErrorType for Sim<PHYS> {
            type Error = NorFlashErrorKind;
        }

        impl<const PHYS: usize> ReadNorFlash for Sim<PHYS> {
            const READ_SIZE: usize = 1;

            fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
                let start = offset as usize;
                let src = self
                    .mem
                    .get(start..start + bytes.len())
                    .ok_or(NorFlashErrorKind::OutOfBounds)?;
                bytes.copy_from_slice(src);
                Ok(())
            }

            fn capacity(&self) -> usize {
                self.mem.len()
            }
        }

        impl<const PHYS: usize> NorFlash for Sim<PHYS> {
            const WRITE_SIZE: usize = WORD;
            const ERASE_SIZE: usize = PHYS;

            fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
                let (from, to) = (from as usize, to as usize);
                if to > self.mem.len() {
                    return Err(NorFlashErrorKind::OutOfBounds);
                }
                if !from.is_multiple_of(PHYS) || !to.is_multiple_of(PHYS) {
                    return Err(NorFlashErrorKind::NotAligned);
                }
                for page in (from..to).step_by(PHYS) {
                    self.tick()?;
                    self.mem[page..page + PHYS].fill(0xFF);
                }
                Ok(())
            }

            fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
                let start = offset as usize;
                if start + bytes.len() > self.mem.len() {
                    return Err(NorFlashErrorKind::OutOfBounds);
                }
                if !start.is_multiple_of(WORD) || !bytes.len().is_multiple_of(WORD) {
                    return Err(NorFlashErrorKind::NotAligned);
                }
                self.tick()?;
                for (cell, byte) in self.mem[start..].iter_mut().zip(bytes) {
                    *cell &= *byte;
                }
                Ok(())
            }
        }

        type Chip<const PHYS: usize> = Mutex<NoopRawMutex, RefCell<Sim<PHYS>>>;

        /// Копия `embassy_embedded_hal::flash::partition::BlockingPartition`
        /// (зависимости в `adapters` нет): те же проверки `new` и те же границы.
        struct Window<'a, F: NorFlash> {
            cell: &'a Mutex<NoopRawMutex, RefCell<F>>,
            offset: u32,
            size: u32,
        }

        impl<'a, F: NorFlash> Window<'a, F> {
            fn new(cell: &'a Mutex<NoopRawMutex, RefCell<F>>, offset: u32, size: u32) -> Self {
                let unit = F::ERASE_SIZE as u32;
                assert!(
                    offset.is_multiple_of(unit),
                    "смещение раздела не кратно странице"
                );
                assert!(
                    size.is_multiple_of(unit),
                    "размер раздела не кратен странице"
                );
                Self { cell, offset, size }
            }
        }

        impl<F: NorFlash> ErrorType for Window<'_, F> {
            type Error = F::Error;
        }

        impl<F: NorFlash> ReadNorFlash for Window<'_, F> {
            const READ_SIZE: usize = F::READ_SIZE;

            fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
                assert!(offset as usize + bytes.len() <= self.size as usize);
                self.cell
                    .lock(|f| f.borrow_mut().read(self.offset + offset, bytes))
            }

            fn capacity(&self) -> usize {
                self.size as usize
            }
        }

        impl<F: NorFlash> NorFlash for Window<'_, F> {
            const WRITE_SIZE: usize = F::WRITE_SIZE;
            const ERASE_SIZE: usize = F::ERASE_SIZE;

            fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
                assert!(to <= self.size);
                self.cell
                    .lock(|f| f.borrow_mut().erase(self.offset + from, self.offset + to))
            }

            fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
                assert!(offset as usize + bytes.len() <= self.size as usize);
                self.cell
                    .lock(|f| f.borrow_mut().write(self.offset + offset, bytes))
            }
        }

        fn chip<const PHYS: usize>() -> Chip<PHYS> {
            Mutex::new(RefCell::new(Sim {
                mem: vec![0xFF; TOTAL],
                budget: usize::MAX,
            }))
        }

        fn image() -> Vec<u8> {
            (0..ACTIVE_LEN).map(|i| (i % 251) as u8).collect()
        }

        fn old() -> Vec<u8> {
            (0..ACTIVE_LEN).map(|i| (i % 241) as u8 ^ 0x5A).collect()
        }

        /// Прошивает `old` в ACTIVE и `image` в DFU и запрашивает обмен.
        fn staged<const PHYS: usize, const SP: usize>(chip: &Chip<PHYS>, state_len: u32) {
            chip.lock(|f| {
                let mem = &mut f.borrow_mut().mem;
                mem[..ACTIVE_LEN as usize].copy_from_slice(&old());
                mem[DFU_OFFSET as usize..(DFU_OFFSET + ACTIVE_LEN) as usize]
                    .copy_from_slice(&image());
            });
            let cell = Mutex::new(RefCell::new(Paged::<_, SP>::new(Shared::new(chip))));
            let mut state = Window::new(&cell, STATE_OFFSET, state_len);
            BlockingFirmwareState::new(&mut state, &mut [0; WORD])
                .mark_updated()
                .expect("запросить обмен");
        }

        /// Один заход `prepare_boot` — разделы собираются заново, как при каждом
        /// сбросе: всё состояние живёт во флеше.
        fn boot<const PHYS: usize, const SP: usize>(
            chip: &Chip<PHYS>,
            state_len: u32,
        ) -> Result<State, BootError> {
            let active = Mutex::new(RefCell::new(Paged::<_, SWAP>::new(Shared::new(chip))));
            let dfu = Mutex::new(RefCell::new(Paged::<_, SWAP>::new(Shared::new(chip))));
            let state = Mutex::new(RefCell::new(Paged::<_, SP>::new(Shared::new(chip))));
            BootLoader::new(BootLoaderConfig {
                active: Window::new(&active, 0, ACTIVE_LEN),
                dfu: Window::new(&dfu, DFU_OFFSET, DFU_LEN),
                state: Window::new(&state, STATE_OFFSET, state_len),
            })
            .prepare_boot(&mut [0; 8])
        }

        fn dump<const PHYS: usize>(chip: &Chip<PHYS>, from: u32, len: u32) -> Vec<u8> {
            chip.lock(|f| f.borrow().mem[from as usize..(from + len) as usize].to_vec())
        }

        /// (а) + (б): обмен доходит до конца, ACTIVE — образ из DFU, прежний
        /// образ — в DFU (со сдвигом на запасную страницу, как в `embassy-boot`);
        /// `assert_partitions` не паникует при странице 1 KiB над физической 128 Б
        /// и STATE в одном физическом секторе.
        #[test]
        fn a_swap_over_a_page_coarser_than_the_physical_one_completes() {
            let chip = chip::<128>();
            staged::<128, 128>(&chip, 128);

            let state = boot::<128, 128>(&chip, 128).expect("обмен");

            assert_eq!(state, State::Swap);
            assert_eq!(dump(&chip, 0, ACTIVE_LEN), image(), "ACTIVE");
            assert_eq!(
                dump(&chip, DFU_OFFSET + SWAP as u32, ACTIVE_LEN),
                old(),
                "прежний образ в DFU"
            );
        }

        /// (в): питание пропадает посреди обмена — в том числе посреди стирания
        /// страницы обмена (часть физических страниц уже стёрта); повторные
        /// заходы с разными бюджетами операций доводят обмен до конца.
        #[test]
        fn an_interrupted_swap_resumes_whatever_the_budget() {
            for budget in [1, 2, 7, 8, 9, 50, 140, 300, 600] {
                let chip = chip::<128>();
                staged::<128, 128>(&chip, 128);

                chip.lock(|f| f.borrow_mut().budget = budget);
                assert!(
                    boot::<128, 128>(&chip, 128).is_err(),
                    "бюджет {budget}: обмен должен был прерваться"
                );

                let mut resumes = 0;
                loop {
                    chip.lock(|f| f.borrow_mut().budget = budget.max(300));
                    resumes += 1;
                    assert!(resumes < 50, "бюджет {budget}: обмен не сходится");
                    if boot::<128, 128>(&chip, 128).is_ok() {
                        break;
                    }
                }
                assert_eq!(dump(&chip, 0, ACTIVE_LEN), image(), "бюджет {budget}");
                assert_eq!(
                    dump(&chip, DFU_OFFSET + SWAP as u32, ACTIVE_LEN),
                    old(),
                    "бюджет {budget}"
                );
            }
        }

        /// После завершённого обмена повторный заход откатывает (приложение не
        /// подтвердило образ): ACTIVE снова прежний — журнал на `2 + 4N` слов
        /// держит и ход вперёд, и ход назад.
        #[test]
        fn a_second_boot_without_confirmation_reverts() {
            let chip = chip::<128>();
            staged::<128, 128>(&chip, 128);
            boot::<128, 128>(&chip, 128).expect("обмен");

            boot::<128, 128>(&chip, 128).expect("откат");

            assert_eq!(dump(&chip, 0, ACTIVE_LEN), old());
        }

        /// (г): журнал ровно `(2 + 4N)` слов достаточен — физическая страница 4 Б
        /// позволяет раздел STATE любой длины в словах.
        #[test]
        fn a_state_of_exactly_the_formula_length_is_enough() {
            let chip = chip::<4>();
            staged::<4, 4>(&chip, STATE_MIN);

            boot::<4, 4>(&chip, STATE_MIN).expect("обмен");

            assert_eq!(dump(&chip, 0, ACTIVE_LEN), image());
        }

        /// (г): на слово меньше — `assert_partitions` паникует до всякой записи.
        #[test]
        #[should_panic(expected = "2 + 4 *")]
        fn a_state_one_word_short_is_refused_by_embassy_boot() {
            let chip = chip::<4>();
            staged::<4, 4>(&chip, STATE_MIN);

            drop(boot::<4, 4>(&chip, STATE_MIN - WORD as u32));
        }

        /// Раздел не по странице обёртки (STATE не кратен 1 KiB, смещение DFU
        /// не кратно) отвергает окно при сборке, как `BlockingPartition::new`.
        #[test]
        #[should_panic(expected = "не кратен")]
        fn a_partition_off_the_wrapper_page_is_refused() {
            let chip = chip::<128>();
            let cell = Mutex::new(RefCell::new(Paged::<_, SWAP>::new(Shared::new(&chip))));
            let _ = Window::new(&cell, 0, 128);
        }

        /// `Paged` не проверяет границы `erase(from, to)` по своей странице:
        /// диапазон, кратный физической странице, но не `ERASE_SIZE` обёртки,
        /// проходит и стирает ровно его; не кратный физической — ошибка флеша.
        #[test]
        fn paged_erase_checks_only_the_physical_alignment() {
            let chip = chip::<128>();
            chip.lock(|f| f.borrow_mut().mem.fill(0));
            let mut paged = Paged::<_, SWAP>::new(Shared::new(&chip));

            paged.erase(128, 256).expect("кратно физической странице");
            assert!(dump(&chip, 128, 128).iter().all(|b| *b == 0xFF));
            assert!(dump(&chip, 0, 128).iter().all(|b| *b == 0));
            assert!(dump(&chip, 256, 128).iter().all(|b| *b == 0));

            assert_eq!(paged.erase(64, 192), Err(NorFlashErrorKind::NotAligned));
        }
    }
}
