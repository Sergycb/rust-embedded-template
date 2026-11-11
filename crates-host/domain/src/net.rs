//! Узел `NET`: ведёт сетевой стек `embassy-net`.
//!
//! Стек — это сокеты, которыми пользуются протоколы, и фоновая работа,
//! которая гоняет пакеты между ними и устройством: `Runner::run`. Её нужно
//! где-то исполнять, и место ей — узел, как любой задаче проекта.
//! Порядок старта, исход и сторож у узлов графа общие для всех.
//! Узел не знает, что за устройство под стеком (Ethernet платы или
//! заглушка) — это параметр фрагмента.
//!
//! `Runner::run` не возвращается; упасть узел может только паникой.

pub use embassy_net::{Runner, Stack};

use defmt_or_log::info;
use embassy_net::driver::Driver;

// Узел объявлен рядом со своей задачей — как остальные. `<R>` — `Runner`
// стека над устройством платы, которого `domain` не видит
// (`NET_FRAG<::domain::net::Runner<'static, bsp::net::Device>>`): параметром —
// сам `Runner`, а не устройство, потому что DSL не принимает у параметра
// trait-границу, а `Runner<'_, D>` требует `D: Driver`; граница живёт в
// сигнатуре задачи. `local` — устройство вправе быть `!Send` (DMA,
// периферия). Без `watchdog:`: узлу нечем отметиться,
// `Runner::run` не отдаёт управление наружу.
//
// Вход — `NetWiring { runner }`: структуру эмитит фрагмент.
supervisor::supervisor_fragment! {
    name: NET_FRAG<R>;
    boot: inputs: struct $crate::net::NetWiring;

    node resources: [
            /// Фоновая работа стека `embassy-net` над устройством платы.
            RUNNER: local R = inputs.runner
        ],
        ctx: $crate::net::NetCtx,
        task: $crate::net::task;
}

/// Узел `NET`: исполняет стек, пока жива прошивка; не возвращается.
///
/// `Runner::run` не отдаёт управление наружу, поэтому узел — ровно
/// «сообщить о старте и гнать пакеты»: ни состояний, ни цикла.
pub async fn task<D: Driver>(ctx: NetCtx<'_, Runner<'static, D>>) {
    info!("net: узел запущен");
    ctx.runner.run().await;
}

#[cfg(test)]
mod tests {
    //! Тесты узла `NET`.

    use core::pin::pin;
    use core::task::{Context, Waker};

    use embassy_net::{Config, StackResources};

    use super::{NetCtx, task};

    /// Узел гоняет стек и не возвращается: над устройством без линка он
    /// ждёт.
    #[test]
    fn drives_the_stack_forever() {
        let device = adapters_hw::net::fake::Device::new([2, 0, 0, 0, 0, 1]);
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
}
