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

#[cfg(test)]
mod tests {
    use core::cell::RefCell;

    use embassy_sync::blocking_mutex::Mutex;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use embedded_storage::nor_flash::{ErrorType, NorFlashErrorKind};
    use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};

    use super::Shared;

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
}
