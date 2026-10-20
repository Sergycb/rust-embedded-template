#![no_std]

// Первым и с `#[macro_use]`: `split_resources!`, который объявляет
// `assign_resources!`, виден `board.rs` только в текстовой области — модуль
// с ним обязан стоять раньше. rustfmt такие модули не переставляет.
#[macro_use]
mod resources;
{% if bank_switch != "" %}
pub mod banks;
{%- endif %}
pub mod board;
{%- if cache != "" %}
pub mod cache;
{%- endif %}
{%- if has_can == "true" %}
pub mod can;
{%- endif %}
{%- if cpu_freq_boost == "true" %}
pub mod clocks;
{%- endif %}
pub mod cpu;
{%- if core_baseline != "true" %}
pub mod cycles;
{%- endif %}
{%- if cache contains "scb_d" %}
pub mod dma_nc;
{%- endif %}
{%- if ota == "true" and ota_store == "external" %}
pub mod external;
{%- endif %}
pub mod fault;
{%- if shared_flash == "true" %}
pub mod flash;
{%- endif %}
{%- if has_modbus == "true" or has_protobuf == "true" %}
pub mod link;
{%- endif %}
{%- if has_net == "true" %}
pub mod net;
{%- endif %}
{%- if ota == "true" %}
pub mod ota;
{%- endif %}
#[cfg(feature = "panic-handler")]
pub mod panic;
{%- if has_net == "true" %}
{%- if rng_irq != "" %}
pub mod rng;
{%- endif %}
{%- endif %}
{%- if has_uart == "true" %}
pub mod serial;
{%- endif %}
{%- if config == "true" %}
pub mod settings;
{%- endif %}
{%- if has_usb == "true" %}
pub mod usb;
{%- endif %}
{%- if graph == "true" %}
pub mod wdg;
{%- endif %}

pub use board::{Board, ChipSpec};
