//! Узел `USB` графа задач: ведёт USB-устройство `embassy-usb`.
//!
//! Устройство — это классы (CDC ACM под протоколом) и фоновая работа,
//! которая отвечает хосту на управляющие запросы и гоняет пакеты:
//! `UsbDevice::run`. Её место — узел графа, как у сети (`domain::net`):
//! порядок старта и исход — общие для всех задач. Узел не знает, что за
//! драйвер под устройством (USB-блок платы или заглушка), — это параметр
//! фрагмента.
//!
//! `UsbDevice::run` не возвращается, поэтому узел запускается один раз
//! (`restart: Never`).

pub use embassy_usb::UsbDevice;

use defmt_or_log::info;
use embassy_usb::driver::Driver;
use supervisor::runtime::TaskExit;

/// Узел `USB`: исполняет устройство, пока жива прошивка; не возвращается.
pub async fn run<D: Driver<'static>>(mut ctx: UsbCtx<'_, UsbDevice<'static, D>>) -> TaskExit {
    ctx.run().await.result
}

// {%- if graph != "true" %}
/// Тот же узел для проекта без графа: спавнится из `main.rs`, а слот, который
/// в графе держит `static`, — локальная переменная кадра, как у
/// [`ota::task`](crate::ota::task).
pub async fn task<D: Driver<'static>>(mut device: UsbDevice<'static, D>) -> TaskExit {
    run(UsbCtx {
        device: &mut device,
        index: 0,
    })
    .await
}
// {%- endif %}

#[fsm::typestate]
impl<D: Driver<'static>> UsbCtx<'_, UsbDevice<'static, D>> {
    fsm::machine! {
        machine Usb mod edges {
            states {
                /// Устройство работает.
                Running,
                /// Недостижимо: `UsbDevice::run` не возвращается. Состояние
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
        info!("domain: узел USB запущен");
        Running
    }

    /// Отвечает хосту и гоняет пакеты классов — бесконечно.
    async fn drive(&mut self, _: Running) -> Down {
        self.device.run().await
    }
}

// Узел объявлен рядом со своей задачей — как `NET_FRAG`. `<U>` — само
// устройство `UsbDevice<'static, D>` над драйвером платы, которого `domain`
// не видит: DSL не принимает у параметра trait-границу, а `UsbDevice<'_, D>`
// требует `D: Driver`, поэтому граница — в сигнатуре задачи. `local` —
// драйвер вправе быть `!Send` (периферия). Без `watchdog:`: `run` не отдаёт
// управление наружу, отметиться нечем.
//
// Вход — `UsbWiring { device }`: структуру эмитит фрагмент.
supervisor::supervisor_fragment! {
    name: USB_FRAG<U>;
    boot: inputs: struct $crate::usb::UsbWiring;

    node deps: [], restart: Never,
        resources: [
            /// USB-устройство с классами протокола над драйвером платы.
            DEVICE: local U = inputs.device
        ],
        ctx: $crate::usb::UsbCtx,
        task: $crate::usb::run;
}

#[cfg(test)]
mod tests {
    use core::pin::pin;
    use core::task::{Context, Waker};

    use embassy_usb::{Builder, Config};

    use super::{UsbCtx, run};

    fn leak<T>(value: T) -> &'static mut T {
        Box::leak(Box::new(value))
    }

    /// Узел исполняет устройство и не возвращается: над драйвером без шины
    /// он ждёт.
    #[test]
    fn drives_the_device_forever() {
        let mut device = Builder::new(
            adapters::usb::Unconnected::default(),
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
        let mut task = pin!(run(ctx));
        assert!(task.as_mut().poll(&mut cx).is_pending());
        assert!(task.poll(&mut cx).is_pending());
    }
}
