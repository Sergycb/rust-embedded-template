#![doc = include_str!("../../../docs/modules/bsp-serial.md")]

/// Поток байтов UART платы — пока заглушка, до драйвера (см. модуль).
pub type Serial = adapters::serial::Unconnected;

/// Собирает UART платы. Блок приходит группой `resources.rs` — заглушка его не
/// трогает, драйвер платы соберётся из него и выводов.
pub(crate) fn new(_resources: crate::resources::SerialResources) -> Serial {
    defmt::warn!("bsp: UART не подключён — узел протокола ждёт впустую");
    adapters::serial::Unconnected
}
