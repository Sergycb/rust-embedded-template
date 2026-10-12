//! Мост blocking → async над общим флешем.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};

use adapters::flash::Shared;

use crate::mem_flash::MemFlash;

#[tokio::test]
async fn forwards_every_operation_to_the_flash_under_the_mutex() {
    let flash: Mutex<NoopRawMutex, _> = Mutex::new(RefCell::new(MemFlash::<64, 16, 4>::filled(0)));
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
