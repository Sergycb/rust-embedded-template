#![cfg_attr(not(test), no_std)]

//! Адаптеры — реализации портов из `ports`, которым не нужен конкретный чип.
//!
//! Правило деления с `crates-cross/bsp`: пока реализации хватает трейтов
//! `embedded-hal`/`embedded-storage` (шина или раздел флеша передаётся
//! параметром) — она лежит здесь и тестируется на хосте на фейке; как только
//! понадобится `embassy_stm32::peripherals`, линкерный символ или статический
//! DMA-буфер — это уже железо, и место ему в `bsp` (docs/architecture.md).
//! `bsp` при этом остаётся обвязкой: строит разделы из символов `memory.x`,
//! отдаёт им псевдонимы (задачи embassy не generic) и кладёт в `Board`.

/// Обновление прошивки поверх `embassy-boot`: разделы `DFU`/`BOOTLOADER_STATE`
/// как два `NorFlash`.
pub mod ota;

/// Мост blocking → async над общим `Flash` под `blocking_mutex` — то, чего
/// ждёт `sequential-storage` от флеша, который делится с OTA.
pub mod flash;

/// Настройки во flash поверх `sequential-storage`: пары «ключ — значение»,
/// переживающие перезапуск и обновление прошивки.
pub mod settings;

// {%- if stream_io == "true" and protocol == "protobuf" %}
/// Кадрирование потока байтов (UART, USB CDC, TCP) в кадры `ports::FrameLink`:
/// COBS и CRC.
pub mod cobs;

// {%- endif %}
// {%- if interface == "uart" %}
/// Заглушка потока байтов в слоте UART — до драйвера платы.
pub mod serial;

// {%- endif %}
// {%- if interface == "tcp" or interface == "udp" %}
/// Сеть поверх `embassy-net`: заглушка устройства, TCP-сервер и UDP-канал.
pub mod net;

// {%- endif %}
// {%- if interface == "usb" %}
/// USB поверх `embassy-usb`: заглушка драйвера и поток CDC ACM.
pub mod usb;

// {%- endif %}
// {%- if interface == "can" %}
/// Заглушка шины в слоте CAN — до контроллера платы.
pub mod can;

// {%- endif %}
// {%- if protocol == "modbus" %}
/// Транспорты Modbus — RTU, TCP, UDP — для порта slave'а `ports::ModbusServer`.
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
