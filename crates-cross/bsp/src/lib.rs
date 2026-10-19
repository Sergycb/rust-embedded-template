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
{%- if interface == "can" %}
pub mod can;
{%- endif %}
{%- if cpu_freq_boost == "true" %}
pub mod clocks;
{%- endif %}
pub mod cpu;
{%- if core_baseline != "true" %}
pub mod cycles;
{%- endif %}
{%- if ota == "true" and ota_store == "external" %}
pub mod external;
{%- endif %}
pub mod fault;
{%- if shared_flash == "true" %}
pub mod flash;
{%- endif %}
{%- if protocol_link == "true" %}
pub mod link;
{%- endif %}
{%- if interface == "tcp" or interface == "udp" %}
pub mod net;
{%- endif %}
{%- if ota == "true" %}
pub mod ota;
{%- endif %}
{%- if interface == "tcp" or interface == "udp" %}
{%- if rng_irq != "" %}
pub mod rng;
{%- endif %}
{%- endif %}
{%- if interface == "uart" %}
pub mod serial;
{%- endif %}
{%- if config == "true" %}
pub mod settings;
{%- endif %}
{%- if interface == "usb" %}
pub mod usb;
{%- endif %}
{%- if graph == "true" %}
pub mod wdg;
{%- endif %}

pub use board::{Board, ChipSpec};
