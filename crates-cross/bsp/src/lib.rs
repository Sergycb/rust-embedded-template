#![no_std]

pub mod board;
{%- if config == "true" %}
pub mod config;
{%- endif %}
{%- if ota == "true" %}
pub mod ota;
{%- endif %}
pub mod wdg;

pub use board::Board;
{%- if ota == "true" or config == "true" %}
pub use board::FlashMutex;
{%- endif %}
