#![doc = include_str!("../../../docs/modules/bsp-net.md")]

use embassy_net::{Config, DhcpConfig, StackResources};
use static_cell::ConstStaticCell;

/// Сокетов в стеке — переменная среды `NET_SOCKETS` из `[env]`
/// `crates-cross/.cargo/config.toml`. Считает её генерация: DHCP-клиент, по
/// сокету на сетевой транспорт{% if net_services != "" %} и на каждое соединение сетевых служб
/// приложения ({% if net_services contains "rest" %}пул REST{% endif %}{% if net_services contains "ssh" %}{% if net_services contains "rest" %}, {% endif %}консоль SSH{% endif %}{% if net_services contains "mqtt" %}{% if net_services contains "rest" or net_services contains "ssh" %}, {% endif %}клиент MQTT{% endif %}){% endif %}. Сколько сокетов откроет
/// приложение, плата не знает и знать не должна; слот — около 500 байт
/// оперативной памяти, поэтому без запаса: добавите сокет (или фичу `dns` у
/// `embassy-net` — она тоже занимает слот) — прибавьте один в `[env]`.
const SOCKETS: usize = match usize::from_str_radix(env!("NET_SOCKETS"), 10) {
    Ok(sockets) => sockets,
    Err(_) => panic!("NET_SOCKETS в [env] crates-cross/.cargo/config.toml — не число"),
};

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
{%- if rng_irq == "" %}

/// Затравка стека из заводского номера — для чипов без RNG. Не случайна: у
/// одной платы она одна и та же после каждого сброса.
pub(crate) fn seed_from_uid(uid: [u8; 12]) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&uid[..8]);
    u64::from_le_bytes(bytes) ^ u64::from(u32::from_le_bytes([uid[8], uid[9], uid[10], uid[11]]))
}
{%- endif %}
