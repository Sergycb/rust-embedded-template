#![doc = include_str!("../../../docs/modules/bsp-ethernet.md")]

use embassy_net::driver::Driver;
use embassy_net::{Config, DhcpConfig, StackResources};
use static_cell::ConstStaticCell;

/// Сокетов в стеке: DHCP-клиент, по сокету на сетевой транспорт и на каждое
/// соединение сетевых служб приложения{% if net_services != "" %} ({% if net_services contains "rest" %}пул REST{% endif %}{% if net_services contains "ssh" %}{% if net_services contains "rest" %}, {% endif %}консоль SSH{% endif %}{% if net_services contains "mqtt" %}{% if net_services contains "rest" or net_services contains "ssh" %}, {% endif %}клиент MQTT{% endif %}){% endif %}. Без
/// запаса: откроете ещё сокет — прибавьте его здесь (модуль, «Адрес, MAC и
/// порты»).
pub const SOCKETS: usize = DHCP + LINKS + SERVICES;
{%- assign single = 0 %}
{%- if net_services contains "ssh" %}{% assign single = single | plus: 1 %}{% endif %}
{%- if net_services contains "mqtt" %}{% assign single = single | plus: 1 %}{% endif %}

/// Слот DHCP-клиента.
const DHCP: usize = 1;
/// Сетевых транспортов (`tcp`, `udp`) — число из генерации; транспорт без
/// протокола тоже держит слот — сокет под него открывает приложение.
const LINKS: usize = {{net_sockets}};
/// Соединений сетевых служб: {% if net_services contains "ssh" or net_services contains "mqtt" %}по одному у {% if net_services contains "ssh" %}консоли SSH{% endif %}{% if net_services contains "ssh" and net_services contains "mqtt" %} и {% endif %}{% if net_services contains "mqtt" %}клиента MQTT{% endif %}{% if net_services contains "rest" %}, {% endif %}{% endif %}{% if net_services contains "rest" %}копия пула REST — соединение{% endif %}{% if net_services == "" %}служб нет{% endif %}.
const SERVICES: usize = {% if net_services contains "rest" %}{% if single > 0 %}{{single}} + {% endif %}HTTP_CONNECTIONS{% else %}{{single}}{% endif %};
{%- if net_services contains "rest" %}
/// Копий пула узлов `HTTP` — `HTTP_CONNECTIONS` в `[env]`
/// `crates-cross/.cargo/config.toml`, то же число читает `app::net::rest`.
const HTTP_CONNECTIONS: usize = match usize::from_str_radix(env!("HTTP_CONNECTIONS"), 10) {
    Ok(connections) => connections,
    Err(_) => panic!("HTTP_CONNECTIONS в [env] crates-cross/.cargo/config.toml — не число"),
};
{%- endif %}

/// Сетевой стек платы — на нём открывают свои сокеты транспорты протоколов
/// и сетевые службы приложения.
pub type Stack = embassy_net::Stack<'static>;

/// Фоновая работа стека (узел `NET`) и сам стек — их рождает один
/// `embassy_net::new`.
pub type Net<D> = adapters_hw::net::Net<D>;

// Память стека: `embassy_net::new` держит её `&'static mut`. `ConstStaticCell`
// — значения известны при компиляции и лежат в `.bss`, а не собираются на
// стеке и копируются при старте.
static RESOURCES: ConstStaticCell<StackResources<SOCKETS>> =
    ConstStaticCell::new(StackResources::new());

/// Собирает стек над устройством чипа: адрес — по DHCP, затравка случайных
/// чисел стека (порты, номера последовательностей TCP) — `seed`.
///
/// MAC и само устройство — из чипа (`cpu::eth`): плата о них не знает.
pub(crate) fn new<D: Driver + 'static>(device: D, seed: u64) -> Net<D> {
    let config = Config::dhcpv4(DhcpConfig::default());
    let (stack, runner) = embassy_net::new(device, config, RESOURCES.take(), seed);
    Net { runner, stack }
}
