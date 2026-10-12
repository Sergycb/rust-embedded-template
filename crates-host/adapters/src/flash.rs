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

#[cfg(test)]
mod tests {
    use core::cell::RefCell;

    use embassy_sync::blocking_mutex::Mutex;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};

    use super::{Paged, Shared};
    use crate::mem_flash::MemFlash;

    #[tokio::test]
    async fn forwards_every_operation_to_the_flash_under_the_mutex() {
        let flash: Mutex<NoopRawMutex, _> =
            Mutex::new(RefCell::new(MemFlash::<64, 16, 4>::filled(0)));
        let mut shared = Shared::new(&flash);

        assert_eq!(shared.capacity(), 64);
        assert_eq!(
            <Shared<'_, Mutex<NoopRawMutex, RefCell<MemFlash<64, 16, 4>>>> as NorFlash>::WRITE_SIZE,
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

    /// `Paged` меняет только объявленную страницу: операции доходят до флеша
    /// с теми же границами — и по блокирующим трейтам (разделы `embassy-boot`),
    /// и по асинхронным (`sequential-storage`).
    #[tokio::test]
    async fn paged_declares_its_own_page_and_forwards_the_rest() {
        type Flash = Mutex<NoopRawMutex, RefCell<MemFlash<64, 16, 4>>>;
        type Page32<'a> = Paged<Shared<'a, Flash>, 32>;
        let flash: Flash = Mutex::new(RefCell::new(MemFlash::filled(0)));

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
