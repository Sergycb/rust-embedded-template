#![doc = include_str!("../../../docs/modules/bsp-net.md")]

use adapters::net::Unconnected;
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
pub type Device = Unconnected;

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
        Unconnected::new(adapters::net::mac_from_uid(uid)),
        Config::dhcpv4(DhcpConfig::default()),
        RESOURCES.take(),
        seed,
    );
    Net { runner, stack }
}
