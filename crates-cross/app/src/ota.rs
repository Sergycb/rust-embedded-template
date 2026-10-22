//! Тип адаптера обновления — порт `FirmwareUpdate` (`adapters::ota`) над
//! разделами платы `DFU`/`BOOTLOADER_STATE` (`bsp::ota::Partitions`). Собирает
//! его узел OTA (`domain::ota`, порт `Assemble`).

/// Адаптер обновления. Псевдоним: тип слота узла OTA должен быть конкретным, а
/// адаптер приходит из `adapters` generic.
pub type Updater = adapters::ota::Updater<bsp::ota::Dfu, bsp::ota::State, {{signed}}>;
