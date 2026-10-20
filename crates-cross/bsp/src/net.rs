#![doc = include_str!("../../../docs/modules/bsp-net.md")]

{%- if has_udp == "true" %}
/// Описание датаграммы в буфере UDP-сокета — для буферов, которые приложение
/// отдаёт сокету.
pub use embassy_net::udp::PacketMetadata;
{%- endif %}
use embassy_net::{Config, DhcpConfig, StackResources};
use static_cell::ConstStaticCell;

/// Сокетов в стеке: DHCP-клиент и по сокету на сетевой транспорт — у
/// транспорта с протоколом его открывает `bsp::link`, без протокола — ваш код
/// на `Board::net`. Слот — около 500 байт оперативной памяти, поэтому без
/// запаса: добавите ещё сокет (или фичу `dns` у `embassy-net` — она тоже
/// занимает слот) — прибавьте один здесь.
const SOCKETS: usize = {{ net_sockets | plus: 1 }};

// Память стека: `embassy_net::new` держит её `&'static mut`. `ConstStaticCell`
// — значения известны при компиляции и лежат в `.bss`, а не собираются на
// стеке и копируются при старте.
static RESOURCES: ConstStaticCell<StackResources<SOCKETS>> =
    ConstStaticCell::new(StackResources::new());

/// Сетевое устройство платы — пока заглушка, до Ethernet (см. модуль).
pub type Device = crate::stub::NetDevice;

/// Сетевой стек, на котором приложение открывает сокет протокола. Псевдоним —
/// чтобы `app` не называл `embassy-net`.
pub type Stack = embassy_net::Stack<'static>;

/// Сеть платы: фоновая работа стека для узла `NET` и сам стек. Сокеты
/// протоколов и их порты — дело `bsp::link`: порт задаёт протокол, а не плата.
pub type Net = adapters::net::Net<Device>;

/// Собирает стек над устройством платы. Адрес — по DHCP; MAC — из
/// заводского номера чипа `uid`, затравка случайных чисел стека — `seed`.
/// Блок Ethernet приходит группой `resources.rs` — заглушка его не трогает,
/// драйвер платы соберётся из него и выводов.
pub(crate) fn new(uid: [u8; 12], seed: u64, _resources: crate::resources::EthResources) -> Net {
    defmt::warn!("bsp: Ethernet не подключён — сеть без линка, протокол ждёт впустую");
    let (stack, runner) = embassy_net::new(
        Device::new(mac_from_uid(uid)),
        Config::dhcpv4(DhcpConfig::default()),
        RESOURCES.take(),
        seed,
    );
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

/// Затравка стека (порты, номера последовательностей TCP) из заводского
/// номера — для чипов без RNG. Не случайна: у одной платы она одна и та же
/// после каждого сброса.
pub(crate) fn seed_from_uid(uid: [u8; 12]) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&uid[..8]);
    u64::from_le_bytes(bytes) ^ u64::from(u32::from_le_bytes([uid[8], uid[9], uid[10], uid[11]]))
}
{%- endif %}
