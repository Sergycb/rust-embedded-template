//! Узел `NET` графа задач: ведёт сетевой стек `embassy-net`.
//!
//! Стек — это сокеты, которыми пользуются протоколы (TCP-сервер Modbus,
//! UDP-канал HOST), и фоновая работа, которая гоняет пакеты между ними и
//! устройством: `Runner::run`. Её нужно где-то исполнять, и место ей — узел
//! графа, как любой задаче проекта: порядок старта, исход и сторож — общие
//! для всех. Узел не знает, что за устройство под стеком (Ethernet платы или
//! заглушка) — это параметр фрагмента.
//!
//! `Runner::run` не возвращается, поэтому узел запускается один раз
//! (`restart: Never`): перезапускать нечего, а упасть он может только
//! паникой.

pub use embassy_net::{Runner, Stack};

use defmt_or_log::info;
use embassy_net::driver::Driver;
use supervisor::runtime::TaskExit;

#[fsm::typestate]
impl<D: Driver> NetCtx<'_, Runner<'static, D>> {
    fsm::machine! {
        machine Net mod edges {
            states {
                /// Стек работает.
                Running,
                /// Недостижимо: `Runner::run` не возвращается. Состояние
                /// нужно автомату — без выхода задача не вернула бы `TaskExit`.
                exit Down { result: TaskExit },
            }
            start => begin -> Running;
            transitions {
                Running => drive -> Down,
            }
        }
    }

    #[expect(
        clippy::unused_self,
        reason = "сигнатуру действия задаёт таблица `#[fsm::typestate]`"
    )]
    fn begin(&mut self) -> Running {
        info!("domain: узел NET запущен");
        Running
    }

    /// Гоняет пакеты между сокетами и устройством — бесконечно.
    async fn drive(&mut self, _: Running) -> Down {
        self.runner.run().await
    }
}

/// Узел `NET`: исполняет стек, пока жива прошивка; не возвращается.
pub async fn task<D: Driver>(mut ctx: NetCtx<'_, Runner<'static, D>>) -> TaskExit {
    ctx.run().await.result
}

// Узел объявлен рядом со своей задачей — как остальные. `<R>` — `Runner`
// стека над устройством платы, которого `domain` не видит
// (`NET_FRAG<::domain::net::Runner<'static, bsp::net::Device>>`): параметром —
// сам `Runner`, а не устройство, потому что DSL не принимает у параметра
// trait-границу, а `Runner<'_, D>` требует `D: Driver`; граница живёт в
// сигнатуре задачи. `local` — устройство вправе быть `!Send` (DMA,
// периферия). Без `watchdog:` — по правилу `heartbeat::task`: узлу нечем отметиться,
// `Runner::run` не отдаёт управление наружу.
//
// Вход — `NetWiring { runner }`: структуру эмитит фрагмент.
supervisor::supervisor_fragment! {
    name: NET_FRAG<R>;
    boot: inputs: struct $crate::net::NetWiring;

    node deps: [], restart: Never,
        resources: [
            /// Фоновая работа стека `embassy-net` над устройством платы.
            RUNNER: local R = inputs.runner
        ],
        ctx: $crate::net::NetCtx,
        task: $crate::net::task;
}

#[cfg(test)]
mod tests {
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
}
