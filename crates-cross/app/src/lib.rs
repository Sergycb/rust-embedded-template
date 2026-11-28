#![doc = include_str!("../../../docs/modules/app.md")]
#![no_std]
#![allow(
    missing_debug_implementations,
    reason = "pub-типы крейта читают только бинарники пакета, а shadow-rs генерирует их без Debug"
)]

// Библиотека пакета — всё, что не запуск: узлы{% if graph == "true" %} (граф){% else %} (задачи){% endif %}, паникёр и логгер,
// обработчик `HardFault`, цикл исполнителя{% if ota == "true" %}, свойства образа
// объявляет бинарник константой `IMAGE` (`domain::ota::Image`){% endif %}. Запуск — `#[entry]`
// бинарника: рабочего `app` (`src/main.rs`){% if test_image == "true" %} и тестового
// `test-ota` (`src/bin/test-ota.rs`), у каждого свой{% endif %}. Паникёр, логгер и обработчики
// фолтов живут здесь и попадают в бинарник ровно по разу — docs/diagnostics.md,
// «Два паникёра, один на сборку»{% if ota == "true" %}; docs/modules/app.md{% endif %}.
pub mod fault;
{%- if graph == "true" %}
mod graph;
{%- endif %}
pub mod layout;
{%- if has_app_net == "true" %}
mod net;
{%- endif %}
pub mod panic;
{%- if has_modbus == "true" %}
mod registers;
{%- endif %}
pub mod storage;
{%- if graph != "true" %}
mod tasks;
{%- endif %}

// Узлы бинарник только спавнит: модуль {% if graph == "true" %}графа{% else %}задач{% endif %} закрыт, наружу — `spawn_all`{% if graph == "true" and ota == "true" %} и
// его `boot:`-объект{% endif %}.
{%- if graph == "true" %}
pub use graph::{% if ota == "true" %}{Boot, spawn_all}{% else %}spawn_all{% endif %};
{%- else %}
pub use tasks::spawn_all;
{%- endif %}

// Версия пакета и коммит, из которого собран образ (`build::PKG_VERSION`,
// `build::SHORT_COMMIT`), — модуль `build`, который `shadow-rs` подставляет
// из build.rs.
shadow_rs::shadow!(build);

// Псевдонимы чипа и платы: задачи embassy не generic, и конкретные типы им
// приходится называть по имени. Один набор на `app` и граф — сменился чип,
// меняется он здесь.
{%- assign xs = links | split: "," %}

/// Метка мьютекса прошивки: `Cpu::Mutex` и замки `app`.
pub type Mutex = cpu::ThreadModeRawMutex;

/// Чип этого проекта: один alias на `app` и граф.
pub type Chip = cpu::Cpu<Mutex>;

/// Плата над ним.
pub type Board = bsp::Board<Chip>;

/// Сведения о чипе.
pub type CpuInfo = cpu::CpuInfo;

/// Система чипа: перезагрузка и причина сброса.
pub type Reboot = cpu::System;
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
{%- assign parts = f[4] | split: "_" %}

/// Устройство `embassy-usb` транспорта `{{ f[4] }}`.
pub type {% for part in parts %}{{ part | capitalize }}{% endfor %}Device = bsp::usb::{{ f[4] }}::Device<<Chip as ports::Cpu>::{% for part in parts %}{{ part | capitalize }}{% endfor %}>;

/// Поток CDC транспорта `{{ f[4] }}`.
pub type {% for part in parts %}{{ part | capitalize }}{% endfor %}Cdc = bsp::usb::{{ f[4] }}::Cdc<<Chip as ports::Cpu>::{% for part in parts %}{{ part | capitalize }}{% endfor %}>;
{%- endif %}
{%- endfor %}
{%- if has_net == "true" %}

/// Сетевой стек платы (фоновая работа и сокеты) — узел `NET`.
pub type Net = bsp::ethernet::Net<<Chip as ports::Cpu>::Ethernet>;

/// Устройство Ethernet чипа — узел `NET` получает его внутри стека.
pub type EthernetDevice = <Chip as ports::Cpu>::Ethernet;
{%- endif %}
