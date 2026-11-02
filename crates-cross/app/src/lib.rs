#![doc = include_str!("../../../docs/modules/app.md")]
#![no_std]
#![allow(
    missing_debug_implementations,
    reason = "крейт ничего не экспортирует наружу, а shadow-rs генерирует pub-типы без Debug"
)]

// Тело прошивки — здесь, в библиотеке пакета, а бинарник — только точка
// входа: `#[entry]` рабочего `app` (`src/main.rs`){% if ota == "true" %} и тестового `app-unconfirmed`
// (`src/bin/app-unconfirmed.rs`){% endif %} зовёт [`run`]. Паникёр, логгер и обработчики
// фолтов живут здесь и попадают в бинарник ровно по разу — docs/modules/app.md.
mod cpu_load;
mod fault;
{%- if graph == "true" %}
mod graph;
{%- endif %}
{%- if protocol_link == "true" %}
mod link;
{%- endif %}
{%- if has_modbus == "true" %}
mod modbus;
{%- endif %}
{%- if net_services != "" %}
mod net;
{%- endif %}
mod panic;
mod run;
{%- if graph != "true" %}
mod tasks;
{%- endif %}
{%- if ota == "true" %}
mod version;
{%- endif %}

pub use run::run;

// Версия пакета и коммит, из которого собран образ (`build::PKG_VERSION`,
// `build::SHORT_COMMIT`), — модуль `build`, который `shadow-rs` подставляет
// из build.rs.
shadow_rs::shadow!(build);
