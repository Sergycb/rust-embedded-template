#![doc = include_str!("../../../docs/modules/bsp-net.md")]

{%- if interface == "tcp" %}
use adapters::net::{TcpServer, Unconnected};
{%- else %}
use adapters::net::{UdpLink, Unconnected};
use embassy_net::udp::PacketMetadata;
{%- endif %}
use embassy_net::{Config, DhcpConfig, Runner, StackResources};
use static_cell::ConstStaticCell;

/// Сетевое устройство платы — пока заглушка, до Ethernet (см. модуль).
pub type Device = Unconnected;

/// Порт, на котором протокол ждёт хост: {% if protocol == "modbus" %}502 — стандартный порт Modbus
/// TCP и UDP{% else %}5000 — выбран шаблоном, у protobuf стандартного нет{% endif %}.
pub const PORT: u16 = {% if protocol == "modbus" %}502{% else %}5000{% endif %};

/// Сеть платы: фоновая работа стека для узла `NET` и сокет протокола.
pub struct Net {
    /// Фоновая работа стека — уезжает в узел `NET` графа.
    pub runner: Runner<'static, Device>,
{%- if interface == "tcp" %}
    /// TCP-сервер на [`PORT`] — поток байтов под транспортом протокола.
    pub tcp: TcpServer,
{%- else %}
    /// UDP-канал на [`PORT`] — датаграммы протокола.
    pub udp: UdpLink,
{%- endif %}
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

/// Буфер сокета протокола — с запасом на самый длинный кадр Modbus TCP
/// (260 байт) и сообщение protobuf (`HOST_MESSAGE` в графе).
const SOCKET_BUFFER: usize = 1024;

// Память стека и сокета протокола: `embassy_net::new` и сокет держат её
// `&'static mut`. `ConstStaticCell` — значения известны при компиляции и
// лежат в `.bss`, а не собираются на стеке и копируются при старте.
static RESOURCES: ConstStaticCell<StackResources<SOCKETS>> =
    ConstStaticCell::new(StackResources::new());
static RX: ConstStaticCell<[u8; SOCKET_BUFFER]> = ConstStaticCell::new([0; SOCKET_BUFFER]);
static TX: ConstStaticCell<[u8; SOCKET_BUFFER]> = ConstStaticCell::new([0; SOCKET_BUFFER]);
{%- if interface == "udp" %}
static RX_META: ConstStaticCell<[PacketMetadata; 4]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 4]);
static TX_META: ConstStaticCell<[PacketMetadata; 4]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 4]);
{%- endif %}

/// Собирает стек над устройством платы. Адрес — по DHCP; MAC — из
/// заводского номера чипа `uid`, затравка случайных чисел стека — `seed`.
pub(crate) fn new(uid: [u8; 12], seed: u64) -> Net {
    defmt::warn!("bsp: Ethernet не подключён — сеть без линка, протокол ждёт впустую");
    let (stack, runner) = embassy_net::new(
        Unconnected::new(mac(uid)),
        Config::dhcpv4(DhcpConfig::default()),
        RESOURCES.take(),
        seed,
    );
{%- if interface == "tcp" %}
    let tcp = TcpServer::new(stack, PORT, RX.take(), TX.take());
    Net { runner, tcp }
{%- else %}
    let udp = UdpLink::new(
        stack,
        PORT,
        RX_META.take(),
        RX.take(),
        TX_META.take(),
        TX.take(),
    );
    Net { runner, udp }
{%- endif %}
}

/// MAC из заводского номера: локально администрируемый (бит 1 первого
/// байта), без группового (бит 0) — у каждой платы свой и постоянный. Все
/// 12 байт номера свёрнуты в 5: выброшенный байт сделал бы MAC общим у
/// чипов, которые различаются только им. Свой адрес от производителя —
/// замените здесь.
const fn mac(uid: [u8; 12]) -> [u8; 6] {
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
/// номера — у чипа нет RNG. Не случайна: у одной платы она одна и та же после
/// каждого сброса.
pub(crate) fn uid_seed(uid: [u8; 12]) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&uid[..8]);
    u64::from_le_bytes(bytes) ^ u64::from(u32::from_le_bytes([uid[8], uid[9], uid[10], uid[11]]))
}
{%- endif %}
