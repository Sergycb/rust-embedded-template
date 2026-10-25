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

// Слотов в стеке не меньше, чем сокетов открывают: DHCP-клиент, сетевые
// транспорты (число из генерации) и службы. `NET_SOCKETS` и
// `HTTP_CONNECTIONS` в `[env]` правят руками порознь — без этой проверки
// рассинхрон проявился бы паникой `embassy-net` на плате.
{%- assign single = 0 %}
{%- if net_services contains "ssh" %}{% assign single = single | plus: 1 %}{% endif %}
{%- if net_services contains "mqtt" %}{% assign single = single | plus: 1 %}{% endif %}
/// Слот DHCP-клиента.
const DHCP: usize = 1;
/// Сетевых транспортов — число из генерации.
const LINKS: usize = {{net_sockets}};
/// Служб с одним сокетом: консоль SSH, клиент MQTT.
const SINGLE: usize = {{single}};
const _: () = assert!(
    bsp::net::SOCKETS >= DHCP + LINKS + SINGLE{% if net_services contains "rest" %} + rest::CONNECTIONS{% endif %},
    "NET_SOCKETS в [env] crates-cross/.cargo/config.toml меньше, чем сокетов открывают транспорты и службы"
);
