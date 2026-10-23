//! Сетевые службы приложения: то, что `domain` получает входом узла, а знает
//! только этот проект, — по модулю на службу. Стек, на котором службы открывают
//! сокеты, собирает плата (`bsp::net`), и сокеты под них он заводит по числу из
//! генерации (`NET_SOCKETS`).
{% if net_services contains "mqtt" %}
pub mod mqtt;
{%- endif %}
{%- if net_services contains "rest" %}
pub mod rest;
{%- endif %}
{%- if net_services contains "ssh" %}
pub mod ssh;
{%- endif %}
