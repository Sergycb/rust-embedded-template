#![no_std]
{% if bank_switch != "" %}
pub mod banks;
{%- endif %}
pub mod board;
{%- if interface == "can" %}
pub mod can;
{%- endif %}
{%- if cpu_freq_boost == "true" %}
pub mod clocks;
{%- endif %}
{%- if config == "true" %}
pub mod config;
{%- endif %}
{%- if ota == "true" and ota_store == "external" %}
pub mod external;
{%- endif %}
{%- if interface == "tcp" or interface == "udp" %}
pub mod net;
{%- endif %}
{%- if ota == "true" %}
pub mod ota;
{%- endif %}
{%- if rng_irq != "" %}
pub mod rng;
{%- endif %}
{%- if interface == "uart" %}
pub mod serial;
{%- endif %}
{%- if interface == "usb" %}
pub mod usb;
{%- endif %}
pub mod wdg;

pub use board::{Board, ChipSpec};
