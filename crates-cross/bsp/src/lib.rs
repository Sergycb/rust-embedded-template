#![no_std]
{% if bank_switch != "" %}
pub mod banks;
{%- endif %}
pub mod board;
{%- if cpu_freq_boost == "true" %}
pub mod clocks;
{%- endif %}
{%- if config == "true" %}
pub mod config;
{%- endif %}
{%- if ota == "true" and ota_store == "external" %}
pub mod external;
{%- endif %}
{%- if ota == "true" %}
pub mod ota;
{%- endif %}
{%- if interface == "uart" %}
pub mod serial;
{%- endif %}
pub mod wdg;

pub use board::{Board, ChipSpec};
