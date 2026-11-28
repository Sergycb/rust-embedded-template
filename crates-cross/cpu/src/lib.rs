#![no_std]
//! Чип и его распиновка: HAL, периферия и сырые драйверы, из которых плата
//! (`bsp`) собирает свои рычаги.
//!
//! Крейт знает `embassy-stm32`, но не знает ни раскладки памяти, ни разделов,
//! ни OTA, ни хранилища настроек: это решения проекта, и живут они в `app` и
//! `boot`. Наружу `cpu` отдаёт [`Cpu`] — один объект, единственный
//! `ports::Cpu::split` и статики, к которым `app` возвращается дешёвыми
//! `Copy`-ручками (`flash::Flash`, `external::ExtFlash`).
//!
//! `bsp` видит только `ports::Cpu` и от `cpu` не зависит; `app`/`boot`
//! собирают `cpu::init()` над рычагами платы.

// Первым и с `#[macro_use]`: `split_resources!`, который объявляет
// `assign_resources!`, виден `cpu.rs` только в текстовой области — модуль с
// ним обязан стоять раньше. rustfmt такие модули не переставляет.
//
// Приватный: группы — распиновка чипа для `cpu::init`, а не API крейта.
#[macro_use]
mod wiring;
{%- if bank_switch != "" %}
pub mod banks;
{%- endif %}
{%- if cache != "" %}
pub mod cache;
{%- endif %}
{%- if has_can == "true" %}
pub mod can;
{%- endif %}
{%- if cpu_freq_boost == "true" %}
pub mod clocks;
{%- endif %}
{%- if has_uart == "true" %}
pub mod com;
{%- endif %}
pub mod cpu;
{%- if cache contains "scb_d" %}
pub mod dma_nc;
{%- endif %}
{%- if config_store == "eeprom" %}
pub mod eeprom;
{%- endif %}
{%- if has_net == "true" %}
pub mod eth;
{%- endif %}
{%- if external_flash == "true" %}
pub mod external;
{%- endif %}
pub mod flash;
{%- if has_net == "true" %}
{%- if rng_irq != "" %}
pub mod rng;
{%- endif %}
{%- endif %}
pub mod system;
{%- if has_usb == "true" %}
pub mod usb;
{%- endif %}
{%- if graph == "true" %}
pub mod wdg;
{%- endif %}

pub use cpu::{Cpu, CpuInfo, init, init_with};
pub use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
pub use system::System;
