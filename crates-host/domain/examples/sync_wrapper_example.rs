//! `SyncWrapper<T>` — заставляет компилятор считать `!Sync`-тип `Sync` там,
//! где эксклюзивный доступ гарантирован вручную (только `&mut`, никогда
//! `&`). Актуально, когда `Future` должен быть `Send + Sync`, а внутри
//! держит не-`Sync` тип вроде `RefCell` (многопоточный executor/multi-core
//! сценарий); в типичном однопоточном embassy-проекте не всегда нужен, но
//! zero-cost и zero-dependency, поэтому безопасно иметь под рукой.
//!
//! `cargo run -p domain --example sync_wrapper_example`

use core::cell::RefCell;
use sync_wrapper::SyncWrapper;

struct Shared {
    cell: SyncWrapper<RefCell<u32>>,
}

fn assert_sync<T: Sync>() {}

fn main() {
    assert_sync::<Shared>();

    let mut shared = Shared {
        cell: SyncWrapper::new(RefCell::new(0)),
    };
    *shared.cell.get_mut().get_mut() = 42;
    assert_eq!(*shared.cell.get_mut().get_mut(), 42);
}
