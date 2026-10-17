#![doc = include_str!("../../../docs/modules/bsp-can.md")]

use embassy_stm32::can::Frame;

/// Шина CAN платы — пока заглушка, до контроллера (см. модуль).
pub type Can = adapters::can::Unconnected<Frame>;

/// Собирает шину CAN платы. Блок приходит группой `resources.rs` — заглушка его
/// не трогает, контроллер платы соберётся из него и выводов.
pub(crate) fn new(_resources: crate::resources::CanResources) -> Can {
    defmt::warn!("bsp: CAN не подключён — узел CANopen ждёт впустую");
    Can::default()
}
