#![doc = include_str!("../../../docs/modules/bsp-serial.md")]

/// Поток байтов UART платы — пока заглушка, до драйвера (см. модуль).
pub type Serial = adapters::serial::Unconnected;

/// Собирает UART платы.
pub(crate) fn new() -> Serial {
    defmt::warn!("bsp: UART не подключён — узел протокола ждёт впустую");
    adapters::serial::Unconnected
}
