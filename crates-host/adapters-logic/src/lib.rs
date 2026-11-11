#![cfg_attr(not(test), no_std)]

//! Реализации портов там, где их несколько, и транспорты под ними.
// {%- if cobs_used == "true" or has_modbus == "true" or has_udp == "true" %}
// {%- if cobs_used == "true" or has_udp == "true" %}
//! Каналы протоколов (`ports::FrameLink`, `ports::FrameDuplex`) — над потоком
//! байтов или датаграммой.
// {%- else %}
//! Каналы протоколов (`ports::FrameLink`) — над потоком байтов или датаграммой.
// {%- endif %}
// {%- endif %}
// {%- if modbus_master == "true" %}
//! Транспорты master'а Modbus (`ports::ModbusClient`).
// {%- endif %}
// {%- if has_tcp == "true" %}
//! TCP-сервер (`net::Server`) — поток байтов над сокетом.
// {%- endif %}
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
