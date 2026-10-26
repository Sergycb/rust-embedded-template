//! Тесты узла `USB`.

use core::pin::pin;
use core::task::{Context, Waker};

use embassy_usb::{Builder, Config};

use super::{UsbCtx, task};

fn leak<T>(value: T) -> &'static mut T {
    Box::leak(Box::new(value))
}

/// Узел исполняет устройство и не возвращается: над драйвером без шины
/// он ждёт.
#[test]
fn drives_the_device_forever() {
    let mut device = Builder::new(
        adapters::fake::UsbDriver::default(),
        Config::new(0xC0DE, 0xCAFE),
        leak([0; 64]),
        leak([0; 64]),
        &mut [],
        leak([0; 64]),
    )
    .build();
    let ctx = UsbCtx {
        device: &mut device,
        index: 0,
    };
    let mut cx = Context::from_waker(Waker::noop());
    let mut task = pin!(task(ctx));
    assert!(task.as_mut().poll(&mut cx).is_pending());
    assert!(task.poll(&mut cx).is_pending());
}
