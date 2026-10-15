#![doc = include_str!("../../../docs/modules/bsp-can.md")]

use embassy_stm32::can::enums::BusError;
use embassy_stm32::can::{Can as Controller, Frame};
use ports::{CanBus, LinkError};

/// Шина CAN платы — пока заглушка, до контроллера (см. модуль).
pub type Can = adapters::can::Unconnected<Frame>;

/// Собирает шину CAN платы.
pub(crate) fn new() -> Can {
    defmt::warn!("bsp: CAN не подключён — узел CANopen ждёт впустую");
    Can::default()
}

/// Порт [`CanBus`] поверх CAN-контроллера `embassy-stm32` ({% if can_kind == "fdcan" %}FDCAN{% else %}bxCAN{% endif %}
/// у этого чипа): то, на что меняется заглушка [`Can`], когда у платы
/// появились выводы.
///
/// Ошибка шины на приёме (`BusError`: stuff, form, CRC, нет ACK) — потеря
/// одного кадра, а не отказ: шина та же, следующий кадр может дойти. Шину,
/// которая отвечает ошибкой подряд (bus-off, оторванный трансивер), узел
/// CANopen сам объявляет отказавшей после серии потерь
/// (`domain::canopen::MAX_LOST_IN_A_ROW`).
pub struct Bus {
    can: Controller<'static>,
{%- if can_kind == "bxcan" %}
    /// Включён ли контроллер: у bxCAN выход из режима инициализации —
    /// `async` (ждёт 11 рецессивных бит на линии), а `Board::new` —
    /// синхронный, поэтому включение — при первом обращении.
    enabled: bool,
{%- endif %}
}

// Вручную: у контроллера `embassy-stm32` нет `Debug`, а без него
// `missing_debug_implementations` — ошибка в `lint cross`.
impl core::fmt::Debug for Bus {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Bus").finish_non_exhaustive()
    }
}

impl Bus {
    /// Шина поверх сконфигурированного контроллера
    /// ({% if can_kind == "fdcan" %}`CanConfigurator::into_normal_mode`{% else %}`Can::new` + `set_bitrate`; включит его первый обмен{% endif %}).
    #[must_use]
    pub const fn new(can: Controller<'static>) -> Self {
        Self {
            can,
{%- if can_kind == "bxcan" %}
            enabled: false,
{%- endif %}
        }
    }
{%- if can_kind == "bxcan" %}

    async fn enable(&mut self) {
        if !self.enabled {
            self.can.enable().await;
            self.enabled = true;
        }
    }
{%- endif %}
}

impl CanBus for Bus {
    type Frame = Frame;
    type Error = BusError;

    async fn recv(&mut self) -> Result<Frame, LinkError<BusError>> {
{%- if can_kind == "bxcan" %}
        self.enable().await;
{%- endif %}
        self.can
            .read()
            .await
            .map(|envelope| envelope.frame)
            .map_err(LinkError::Lost)
    }

    async fn send(&mut self, frame: &Frame) -> Result<(), LinkError<BusError>> {
{%- if can_kind == "bxcan" %}
        self.enable().await;
{%- endif %}
        // Возвращает кадр, вытесненный из очереди передачи более
        // приоритетным, — у CANopen такое случается только при переполненной
        // очереди, и вытесненный кадр стек отправит в следующем такте.
        let _displaced = self.can.write(frame).await;
        Ok(())
    }
}
