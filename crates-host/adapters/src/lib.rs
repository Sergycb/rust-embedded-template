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

// {%- if ota == "true" %}
/// Обновление прошивки поверх `embassy-boot`: разделы `DFU`/`BOOTLOADER_STATE`
/// как два `NorFlash`.
pub mod ota;

// {%- endif %}
// {%- if erase_page == "true" or external_flash == "true" %}
/// Вспомогательные типы над `NorFlash`: обёртка со своей страницей стирания и
/// заглушка внешней флеши.
pub mod flash;

// {%- endif %}
// {%- if settings_port == "true" %}
/// Настройки во flash поверх `sequential-storage`: пары «ключ — значение»,
/// переживающие перезапуск и обновление прошивки.
pub mod settings;

// {%- endif %}
// {%- if cobs_used == "true" %}
/// Кадрирование потока байтов (UART, USB CDC, TCP) в кадры `ports::FrameLink`:
/// COBS и CRC.
pub mod cobs;

// {%- endif %}
// {%- if has_uart == "true" %}
/// Заглушка потока байтов в слоте UART — до драйвера платы.
pub mod serial;

// {%- endif %}
// {%- if has_net == "true" %}
/// Сеть поверх `embassy-net`: заглушка устройства, TCP-сервер и UDP-канал.
pub mod net;

// {%- endif %}
// {%- if has_usb == "true" %}
/// USB поверх `embassy-usb`: заглушка драйвера и поток CDC ACM.
pub mod usb;

// {%- endif %}
// {%- if has_can == "true" %}
/// Заглушка шины в слоте CAN — до контроллера платы.
pub mod can;

// {%- endif %}
// {%- if has_modbus == "true" %}
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
