#![cfg_attr(not(test), no_std)]

//! Обёртки над железом и стеками драйверов без конкретного чипа — то, из чего
//! плата (`crates-cross/bsp`) собирает свои рычаги, — и фейки host-тестов.
// {%- if has_usb == "true" %}
//! USB — устройство с потоком CDC (`usb`).
// {%- endif %}
// {%- if has_net == "true" %}
//! Сеть — стек `embassy-net` с фоновой работой (`net`).
// {%- endif %}
// {%- if config_store == "eeprom" %}
//! EEPROM — как флеш с семантикой NOR (`eeprom`).
// {%- endif %}
// {%- if ota == "true" or settings_port == "true" %}
//! Флеш в памяти для host-тестов (`flash`).
// {%- endif %}
//! Драйверы без шины и линка — заглушки платы и фейки host-тестов — живут
//! модулем `fake` в конце модуля, который они подменяют.
//!
//! Деление: нужен чип — `bsp`; обёртка над железом без чипа — здесь;
//! реализации портов, когда их несколько, — `adapters-logic` —
//! docs/architecture.md, «Адаптеры делятся по одному признаку — нужен ли чип».

// {%- if has_net == "true" %}
/// Сеть поверх `embassy-net`: стек платы и его фоновая работа.
pub mod net;

// {%- endif %}
// {%- if has_usb == "true" %}
/// USB поверх `embassy-usb`: поток CDC ACM.
pub mod usb;

// {%- endif %}
// {%- if ota == "true" or settings_port == "true" %}
/// Флеш с семантикой NOR без чипа: флеш в памяти для host-тестов.
pub mod flash;

// {%- endif %}
// {%- if config_store == "eeprom" %}
/// Внутренняя EEPROM (STM32L0/L1) как флеш с семантикой NOR — чтобы поверх
/// неё работало то же хранилище настроек.
pub mod eeprom;
// {%- endif %}
