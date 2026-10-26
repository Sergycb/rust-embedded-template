//! Тесты узла `NET`.

use core::pin::pin;
use core::task::{Context, Waker};

use embassy_net::{Config, StackResources};

use super::{NetCtx, task};

/// Узел гоняет стек и не возвращается: над устройством без линка он
/// ждёт.
#[test]
fn drives_the_stack_forever() {
    let device = adapters::fake::NetDevice::new([2, 0, 0, 0, 0, 1]);
    let resources = Box::leak(Box::new(StackResources::<2>::new()));
    let (_stack, mut runner) = embassy_net::new(device, Config::default(), resources, 1);
    let ctx = NetCtx {
        runner: &mut runner,
        index: 0,
    };
    let mut cx = Context::from_waker(Waker::noop());
    let mut task = pin!(task(ctx));
    assert!(task.as_mut().poll(&mut cx).is_pending());
    assert!(task.poll(&mut cx).is_pending());
}
