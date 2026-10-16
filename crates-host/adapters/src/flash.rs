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
/// Что `ERASE_SIZE` совпадает с настоящими секторами под разделом, обёртка
/// проверить не может: это знает раскладка (`chip-data-gen`), откуда число и
/// приезжает. Ошибись она — стирание вернёт ошибку выравнивания от самого
/// `Flash`, а не сотрёт лишнее.
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
    pub const CAPACITY: u32 = 2 * 4096;
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
    const ERASE_SIZE: usize = 4096;

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
    const ERASE_SIZE: usize = 4096;

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
}
