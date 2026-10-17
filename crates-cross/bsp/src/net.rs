#![doc = include_str!("../../../docs/modules/bsp-net.md")]

use adapters::net::Unconnected;
use embassy_net::{Config, DhcpConfig, Runner, StackResources};
use static_cell::ConstStaticCell;

/// Сетевое устройство платы — пока заглушка, до Ethernet (см. модуль).
pub type Device = Unconnected;

/// Сетевой стек, на котором приложение открывает сокет протокола. Псевдоним —
/// чтобы `app` не называл `embassy-net`.
pub type Stack = embassy_net::Stack<'static>;
{%- if interface == "udp" %}

/// Описание датаграммы в буфере UDP-сокета — для буферов, которые приложение
/// отдаёт сокету.
pub use embassy_net::udp::PacketMetadata;
{%- endif %}

/// Сеть платы: фоновая работа стека для узла `NET` и сам стек. Сокет
/// протокола и его порт — дело приложения (`crates-cross/app/src/graph.rs`):
/// порт задаёт протокол, а не плата.
pub struct Net {
    /// Фоновая работа стека — уезжает в узел `NET` графа.
    pub runner: Runner<'static, Device>,
    /// Стек — на нём приложение открывает сокет протокола.
    pub stack: Stack,
}

// Вручную: у `Runner` нет `Debug`.
impl core::fmt::Debug for Net {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Net").finish_non_exhaustive()
    }
}

/// Сокетов в стеке: DHCP-клиент и сокет протокола. Слот — около 500 байт
/// оперативной памяти, поэтому без запаса: добавите свой сокет (или фичу
/// `dns` у `embassy-net` — она тоже занимает слот) — прибавьте один здесь.
const SOCKETS: usize = 2;

// Память стека: `embassy_net::new` держит её `&'static mut`. `ConstStaticCell`
// — значения известны при компиляции и лежат в `.bss`, а не собираются на
// стеке и копируются при старте.
static RESOURCES: ConstStaticCell<StackResources<SOCKETS>> =
    ConstStaticCell::new(StackResources::new());

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
