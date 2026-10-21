//! Сетевой стек приложения: `embassy-net` над Ethernet платы
//! (`bsp::Drivers::eth`).
//!
//! Число сокетов, адресация и память стека — решение приложения, а не
//! платы: `bsp` отдаёт только устройство. Стек и фоновая работа над ним
//! рождаются одним `embassy_net::new` и расходятся по разным узлам: фон — в
//! узел `NET`, стек — сокетам протоколов (`crate::link`) или вашему коду.

{%- assign xs = links | split: "," %}
{%- assign udp_protocol = "false" %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "udp" and f[3] != "none" %}{% assign udp_protocol = "true" %}{% endif %}
{%- endfor %}
{%- if udp_protocol == "true" %}
/// Описание датаграммы в буфере UDP-сокета — для буферов, которые приложение
/// отдаёт сокету протокола (`crate::link`).
pub use embassy_net::udp::PacketMetadata;
{%- endif %}
use embassy_net::{Config, DhcpConfig, StackResources};
use static_cell::ConstStaticCell;

/// Сокетов в стеке: DHCP-клиент и по сокету на сетевой транспорт — у
/// транспорта с протоколом его открывает `crate::link`, без протокола — ваш
/// код на `Parts::net`. Слот — около 500 байт оперативной памяти, поэтому без
/// запаса: добавите ещё сокет (или фичу `dns` у `embassy-net` — она тоже
/// занимает слот) — прибавьте один здесь.
const SOCKETS: usize = {{ net_sockets | plus: 1 }};

// Память стека: `embassy_net::new` держит её `&'static mut`. `ConstStaticCell`
// — значения известны при компиляции и лежат в `.bss`, а не собираются на
// стеке и копируются при старте.
static RESOURCES: ConstStaticCell<StackResources<SOCKETS>> =
    ConstStaticCell::new(StackResources::new());

{%- if protocol_sockets != "0" %}

/// Сетевой стек, на котором открываются сокеты протоколов (`crate::link`).
pub type Stack = embassy_net::Stack<'static>;
{%- endif %}

/// Фоновая работа стека (узел `NET`) и сам стек.
pub type Net = adapters::net::Net<bsp::net::Device>;

/// Собирает стек над устройством платы. Адрес — по DHCP; затравка случайных
/// чисел стека (порты, номера последовательностей TCP) — `seed`.
pub fn new(device: bsp::net::Device, seed: u64) -> Net {
    let config = Config::dhcpv4(DhcpConfig::default());
    let (stack, runner) = embassy_net::new(device, config, RESOURCES.take(), seed);
    Net { runner, stack }
}
{%- if rng_irq == "" %}

/// Затравка стека из заводского номера — для чипов без RNG. Не случайна: у
/// одной платы она одна и та же после каждого сброса.
pub fn seed_from_uid(uid: [u8; 12]) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&uid[..8]);
    u64::from_le_bytes(bytes) ^ u64::from(u32::from_le_bytes([uid[8], uid[9], uid[10], uid[11]]))
}
{%- endif %}
