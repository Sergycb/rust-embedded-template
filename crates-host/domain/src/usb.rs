//! Узел `USB`: ведёт USB-устройство `embassy-usb`.
//!
//! Устройство — это классы (CDC ACM под протоколом) и фоновая работа,
//! которая отвечает хосту на управляющие запросы и гоняет пакеты:
// {%- if graph == "true" %}
//! `UsbDevice::run`. Её место — узел графа: порядок старта и исход — общие
//! для всех задач. Узел не знает, что за
// {%- else %}
//! `UsbDevice::run`. Её место — свой узел, как у любой задачи проекта. Узел
//! не знает, что за
// {%- endif %}
//! драйвер под устройством (USB-блок платы или заглушка), — это параметр
//! фрагмента.
//!
//! `UsbDevice::run` не возвращается.

pub use embassy_usb::UsbDevice;

use defmt_or_log::info;
use embassy_usb::driver::Driver;

// Узел объявлен рядом со своей задачей — как `NET_FRAG`. `<U>` — само
// устройство `UsbDevice<'static, D>` над драйвером платы, которого `domain`
// не видит: DSL не принимает у параметра trait-границу, а `UsbDevice<'_, D>`
// требует `D: Driver`, поэтому граница — в сигнатуре задачи. `local` —
// драйвер вправе быть `!Send` (периферия). Без `watchdog:`: `run` не отдаёт
// управление наружу, отметиться нечем.
//
// Вход — `UsbWiring { device }`: структуру эмитит фрагмент.
//
// С OTA устройство встаёт на шину после подтверждения образа (`OTA ready`):
// первая загрузка нового или откаченного образа стирает `STATE`, ядро на это
// время может стоять на выборке инструкций, и перечисление, попавшее на
// стирание, хост бракует до переподключения (`domain::ota`, `begin`).
supervisor::supervisor_fragment! {
    name: USB_FRAG<U>;
    boot: inputs: struct $crate::usb::UsbWiring;

    node
        // {%- if ota == "true" %}
        deps: [OTA ready],
        // {%- endif %}
        resources: [
            /// USB-устройство с классами протокола над драйвером платы.
            DEVICE: local U = inputs.device
        ],
        ctx: $crate::usb::UsbCtx,
        task: $crate::usb::task;
}

#[fsm::typestate]
impl<D: Driver<'static>> UsbCtx<'_, UsbDevice<'static, D>> {
    fsm::machine! {
        machine Usb mod edges {
            states {
                /// Устройство работает.
                Running,
                /// Недостижимо: обслуживание не возвращается. Состояние нужно
                /// таблице — строка без ожидания не может остаться в своём состоянии.
                exit Down,
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

/// Узел `USB`: исполняет устройство, пока жива прошивка; не возвращается.
pub async fn task<D: Driver<'static>>(mut ctx: UsbCtx<'_, UsbDevice<'static, D>>) {
    ctx.run().await;
}

#[cfg(test)]
mod tests {
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
}
