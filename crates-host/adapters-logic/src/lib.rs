#![cfg_attr(not(test), no_std)]

//! Реализации портов там, где их несколько: каналы протоколов
//! (`ports::FrameLink`, `ports::FrameDuplex`) над потоком байтов и
//! датаграммой и транспорты master'а Modbus (`ports::ModbusClient`).
//!
//! Чипа здесь нет: поток — любой `embedded_io_async::{Read, Write}`, сокеты —
//! стек `embassy-net`, который строит плата. Обёртки над железом без чипа —
//! в `adapters-hw`; деление — docs/architecture.md, «Адаптеры делятся по одному
//! признаку — нужен ли чип».

// {%- if cobs_used == "true" %}
/// Кадрирование потока байтов в кадры `ports::FrameLink`:
/// COBS и CRC.
pub mod cobs;

// {%- endif %}
// {%- if has_modbus == "true" %}
/// Транспорты Modbus — каналы slave'а (`ports::FrameLink` с
/// кадром «адрес и PDU») для узла протокола `domain::server`.
// {%- if modbus_master == "true" %}
/// Они же реализуют порт master'а `ports::ModbusClient`.
// {%- endif %}
pub mod modbus;

// {%- endif %}
// {%- if has_net == "true" %}
/// Транспорты над сокетами `embassy-net`.
pub mod net;
// {%- endif %}
