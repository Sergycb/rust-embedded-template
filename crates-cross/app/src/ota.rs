//! Адаптер обновления приложения — порт `FirmwareUpdate` (`adapters::ota`)
//! над разделами платы `DFU`/`BOOTLOADER_STATE` (`bsp::ota::Partitions`).

/// Адаптер обновления. Псевдоним: тип слота узла OTA должен быть конкретным, а
/// адаптер приходит из `adapters` generic.
pub type Updater = adapters::ota::Updater<bsp::ota::Dfu, bsp::ota::State, {{signed}}>;

/// Собирает адаптер над разделами. Самый длинный принимаемый образ —
/// `bsp::ota::ACTIVE_LEN`: размер `ACTIVE`, а не `DFU` (почему — у константы).
#[must_use]
pub fn new(partitions: bsp::ota::Partitions) -> Updater {
    Updater::new(partitions.dfu, partitions.state, bsp::ota::ACTIVE_LEN)
}
