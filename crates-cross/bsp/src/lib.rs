#![no_std]

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
pub mod wdg;

pub use board::{Board, ChipSpec};
