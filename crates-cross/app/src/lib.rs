#![doc = include_str!("../../../docs/modules/app.md")]
#![no_std]
#![allow(
    missing_debug_implementations,
    reason = "pub-типы крейта читают только бинарники пакета, а shadow-rs генерирует их без Debug"
)]

// Библиотека пакета — всё, что не запуск: узлы{% if graph == "true" %} (граф){% else %} (задачи){% endif %}, паникёр и логгер,
// обработчик `HardFault`, счётчик сна{% if ota == "true" %}, свойства образа{% endif %}. Запуск — `#[entry]`
// бинарника: рабочего `app` (`src/main.rs`){% if test_image == "true" %} и тестового
// `app-unconfirmed` (`src/bin/app-unconfirmed.rs`), у каждого свой{% endif %}. Паникёр, логгер и обработчики
// фолтов живут здесь и попадают в бинарник ровно по разу — docs/diagnostics.md,
// «Два паникёра, один на сборку»{% if ota == "true" %}; docs/modules/app.md{% endif %}.
{%- if has_console == "true" %}
pub mod console;
{%- endif %}
pub mod cpu_load;
{%- if has_protobuf == "true" or net_services != "" or has_cli == "true" %}
pub mod device;
{%- endif %}
pub mod fault;
{%- if graph == "true" %}
mod graph;
{%- endif %}
{%- if has_modbus == "true" %}
mod modbus;
{%- endif %}
{%- if net_services != "" %}
mod net;
{%- endif %}
pub mod panic;
{%- if graph != "true" %}
mod tasks;
{%- endif %}
{%- if ota == "true" %}
pub mod version;
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
