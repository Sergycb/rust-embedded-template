#![no_std]
//! Плата: рычаги, которые приложение получает от чипа, и интерфейсы над ними.
//!
//! Плата generic по [`ports::Cpu`] и о чипе не знает ничего: ни
//! `embassy-stm32`, ни раскладки памяти, ни разделов. Что именно лежит в
//! полях [`Board`], решает чип (`crates-cross/cpu`), а собирает их
//! [`Board::new`].

pub mod board;
{%- if has_net == "true" %}
pub mod ethernet;
{%- endif %}
{%- if has_usb == "true" %}
pub mod usb;
{%- endif %}

pub use board::Board;
