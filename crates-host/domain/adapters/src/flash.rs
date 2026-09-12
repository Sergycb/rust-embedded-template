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
use embedded_storage::nor_flash::{ErrorType, NorFlash};

/// Ссылка на общий флеш; операции — под его мьютексом, по одной.
pub struct Shared<'a, M: RawMutex, F: NorFlash> {
    flash: &'a Mutex<M, RefCell<F>>,
}

impl<'a, M: RawMutex, F: NorFlash> Shared<'a, M, F> {
    pub fn new(flash: &'a Mutex<M, RefCell<F>>) -> Self {
        Self { flash }
    }
}

impl<M: RawMutex, F: NorFlash> ErrorType for Shared<'_, M, F> {
    type Error = F::Error;
}

impl<M: RawMutex, F: NorFlash> embedded_storage_async::nor_flash::ReadNorFlash
    for Shared<'_, M, F>
{
    const READ_SIZE: usize = F::READ_SIZE;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.flash
            .lock(|flash| flash.borrow_mut().read(offset, bytes))
    }

    fn capacity(&self) -> usize {
        self.flash.lock(|flash| flash.borrow().capacity())
    }
}

impl<M: RawMutex, F: NorFlash> embedded_storage_async::nor_flash::NorFlash for Shared<'_, M, F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.flash.lock(|flash| flash.borrow_mut().erase(from, to))
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.flash
            .lock(|flash| flash.borrow_mut().write(offset, bytes))
    }
}

#[cfg(test)]
mod tests {
    use core::cell::RefCell;

    use embassy_sync::blocking_mutex::Mutex;
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;
    use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};

    use super::Shared;
    use crate::mem_flash::{MemFlash, block_on};

    #[test]
    fn forwards_every_operation_to_the_flash_under_the_mutex() {
        let flash: Mutex<NoopRawMutex, _> =
            Mutex::new(RefCell::new(MemFlash::<64, 16, 4>::filled(0)));
        let mut shared = Shared::new(&flash);

        assert_eq!(shared.capacity(), 64);
        assert_eq!(
            <Shared<'_, NoopRawMutex, MemFlash<64, 16, 4>> as NorFlash>::WRITE_SIZE,
            4
        );

        block_on(shared.erase(16, 32)).expect("стирание");
        block_on(shared.write(16, &[0xA5; 4])).expect("запись");
        let mut back = [0; 8];
        block_on(shared.read(16, &mut back)).expect("чтение");

        assert_eq!(back, [0xA5, 0xA5, 0xA5, 0xA5, 0xFF, 0xFF, 0xFF, 0xFF]);
        assert!(
            flash.lock(|f| f.borrow().mem[..16].iter().all(|b| *b == 0)),
            "соседняя страница не тронута"
        );
    }
}
