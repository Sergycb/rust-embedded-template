#![cfg_attr(not(test), no_std)]

//! Адаптеры — реализации трейтов (портов из `ports` или стандартных) поверх
//! стандартных трейтов, без конкретного чипа, и типы-сборки, которые плата
//! отдаёт целиком.
//!
//! Деление с `crates-cross/bsp` — по одному признаку, нужен ли чип; обёртки,
//! без которых рычаг не собрать, строит и держит в своей памяти сама плата —
//! docs/architecture.md, «Адаптеры делятся по одному признаку — нужен ли чип».

// {%- if ota == "true" %}
/// Обновление прошивки поверх `embassy-boot`: разделы `DFU`/`BOOTLOADER_STATE`
/// как два `NorFlash`.
pub mod ota;

// {%- endif %}
// {%- if settings_port == "true" %}
/// Настройки во flash поверх `sequential-storage`: пары «ключ — значение»,
/// переживающие перезапуск и обновление прошивки.
pub mod settings;

// {%- endif %}
// {%- if cobs_used == "true" %}
/// Кадрирование потока байтов в кадры `ports::FrameLink`:
/// COBS и CRC.
pub mod cobs;

// {%- endif %}
// {%- if has_net == "true" %}
/// Сеть поверх `embassy-net`: транспорты над сокетами стека.
pub mod net;

// {%- endif %}
// {%- if has_usb == "true" %}
/// USB поверх `embassy-usb`: поток CDC ACM.
pub mod usb;

// {%- endif %}
// {%- if has_usb == "true" or has_net == "true" or ota == "true" or config == "true" %}
/// Драйверы без шины и линка и флеш в памяти — для host-тестов (`adapters`,
/// `domain`).
#[cfg(any(test, feature = "fake"))]
pub mod fake;

// {%- endif %}
// {%- if has_modbus == "true" %}
/// Транспорты Modbus — каналы slave'а (`ports::FrameLink` с
/// кадром «адрес и PDU») для узла протокола `domain::server`.
// {%- if modbus_master == "true" %}
/// Они же реализуют порт master'а `ports::ModbusClient`.
// {%- endif %}
pub mod modbus;

// {%- endif %}
// {%- if config_store == "eeprom" %}
/// Внутренняя EEPROM (STM32L0/L1) как флеш с семантикой NOR — чтобы поверх
/// неё работало то же хранилище настроек.
pub mod eeprom;
// {%- endif %}
