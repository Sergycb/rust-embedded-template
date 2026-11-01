#![cfg_attr(not(test), no_std)]

//! Обёртки над железом и стеками драйверов без конкретного чипа — то, из чего
//! плата (`crates-cross/bsp`) собирает свои рычаги: устройство USB с потоком
//! CDC, стек сети, EEPROM как флеш; и драйверы без шины и линка — заглушки
//! платы и фейки host-тестов.
//!
//! Деление: нужен чип — `bsp`; обёртка над железом без чипа — здесь;
//! реализации портов, когда их несколько, — `adapters-logic` —
//! docs/architecture.md, «Адаптеры делятся по одному признаку — нужен ли чип».

// {%- if ota == "true" %}
/// Обновление прошивки поверх `embassy-boot`: разделы `DFU`/`BOOTLOADER_STATE`
/// как два `NorFlash`.
pub mod ota;

// {%- endif %}
// {%- if has_net == "true" %}
/// Сеть поверх `embassy-net`: стек платы и его фоновая работа.
pub mod net;

// {%- endif %}
// {%- if has_usb == "true" %}
/// USB поверх `embassy-usb`: поток CDC ACM.
pub mod usb;

// {%- endif %}
// {%- if has_usb == "true" or has_net == "true" or ota == "true" or settings_port == "true" %}
/// Драйверы без шины и линка и флеш в памяти — для host-тестов (`adapters`,
/// `domain`).
#[cfg(any(test, feature = "fake"))]
pub mod fake;

// {%- endif %}
// {%- if config_store == "eeprom" %}
/// Внутренняя EEPROM (STM32L0/L1) как флеш с семантикой NOR — чтобы поверх
/// неё работало то же хранилище настроек.
pub mod eeprom;
// {%- endif %}
