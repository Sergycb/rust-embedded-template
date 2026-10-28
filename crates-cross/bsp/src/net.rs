#![doc = include_str!("../../../docs/modules/bsp-net.md")]

use embassy_net::{Config, DhcpConfig, StackResources};
use static_cell::ConstStaticCell;

/// Сокетов в стеке: DHCP-клиент, по сокету на сетевой транспорт и на каждое
/// соединение сетевых служб приложения{% if net_services != "" %} ({% if net_services contains "rest" %}пул REST{% endif %}{% if net_services contains "ssh" %}{% if net_services contains "rest" %}, {% endif %}консоль SSH{% endif %}{% if net_services contains "mqtt" %}{% if net_services contains "rest" or net_services contains "ssh" %}, {% endif %}клиент MQTT{% endif %}){% endif %}. Стек
/// строит плата, поэтому и счёт здесь: число слотов сходится с тем, кто их
/// открывает, без отдельной проверки. Слот — около 500 байт оперативной
/// памяти, поэтому без запаса: откроете ещё сокет (или включите фичу `dns` у
/// `embassy-net` — она тоже занимает слот) — прибавьте его здесь.
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

/// Сетевое устройство платы — пока заглушка, до Ethernet (см. модуль).
pub type Device = crate::stub::NetDevice;

/// Сетевой стек платы — на нём открывают свои сокеты транспорты протоколов
/// и сетевые службы приложения.
pub type Stack = embassy_net::Stack<'static>;

/// Фоновая работа стека (узел `NET`) и сам стек — их рождает один
/// `embassy_net::new`.
pub type Net = adapters::net::Net<Device>;

// Память стека: `embassy_net::new` держит её `&'static mut`. `ConstStaticCell`
// — значения известны при компиляции и лежат в `.bss`, а не собираются на
// стеке и копируются при старте.
static RESOURCES: ConstStaticCell<StackResources<SOCKETS>> =
    ConstStaticCell::new(StackResources::new());

/// Собирает стек над устройством платы: MAC — из заводского номера чипа
/// `uid`, адрес — по DHCP, затравка случайных чисел стека (порты, номера
/// последовательностей TCP) — `seed`. Блок Ethernet приходит группой
/// `resources.rs` — заглушка его не трогает, драйвер платы соберётся из него и
/// выводов.
pub(crate) fn new(uid: [u8; 12], seed: u64, _resources: crate::resources::EthResources) -> Net {
    defmt::warn!("bsp: Ethernet не подключён — сеть без линка, протокол ждёт впустую");
    let device = Device::new(mac_from_uid(uid));
    let config = Config::dhcpv4(DhcpConfig::default());
    let (stack, runner) = embassy_net::new(device, config, RESOURCES.take(), seed);
    Net { runner, stack }
}
{%- if rng_irq == "" %}

/// Затравка стека из заводского номера — для чипов без RNG. Не случайна: у
/// одной платы она одна и та же после каждого сброса.
pub(crate) fn seed_from_uid(uid: [u8; 12]) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&uid[..8]);
    u64::from_le_bytes(bytes) ^ u64::from(u32::from_le_bytes([uid[8], uid[9], uid[10], uid[11]]))
}
{%- endif %}

/// MAC из заводского номера чипа: локально администрируемый (бит 1 первого
/// байта), без группового (бит 0) — у каждой платы свой и постоянный. Все
/// 12 байт номера свёрнуты в 5: выброшенный байт сделал бы MAC общим у
/// чипов, которые различаются только им. Свой адрес от производителя —
/// подставьте здесь.
const fn mac_from_uid(uid: [u8; 12]) -> [u8; 6] {
    [
        0x02,
        uid[0] ^ uid[5] ^ uid[10],
        uid[1] ^ uid[6] ^ uid[11],
        uid[2] ^ uid[7],
        uid[3] ^ uid[8],
        uid[4] ^ uid[9],
    ]
}
