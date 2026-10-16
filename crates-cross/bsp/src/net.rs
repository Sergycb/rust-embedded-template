#![doc = include_str!("../../../docs/modules/bsp-net.md")]

{%- if interface == "tcp" %}
use adapters::net::{TcpServer, Unconnected};
{%- else %}
use adapters::net::{UdpLink, Unconnected};
use embassy_net::udp::PacketMetadata;
{%- endif %}
use embassy_net::{Config, DhcpConfig, Runner, StackResources};
use static_cell::StaticCell;

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

/// Собирает стек над устройством платы. Адрес — по DHCP; MAC — из
/// заводского номера чипа `uid`, затравка случайных чисел стека — `seed`.
pub(crate) fn new(uid: [u8; 12], seed: u64) -> Net {
    static RESOURCES: StaticCell<StackResources<SOCKETS>> = StaticCell::new();
    static RX: StaticCell<[u8; SOCKET_BUFFER]> = StaticCell::new();
    static TX: StaticCell<[u8; SOCKET_BUFFER]> = StaticCell::new();
{%- if interface == "udp" %}
    static RX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
    static TX_META: StaticCell<[PacketMetadata; 4]> = StaticCell::new();
{%- endif %}
    defmt::warn!("bsp: Ethernet не подключён — сеть без линка, протокол ждёт впустую");
    let (stack, runner) = embassy_net::new(
        Unconnected::new(mac(uid)),
        Config::dhcpv4(DhcpConfig::default()),
        RESOURCES.init(StackResources::new()),
        seed,
    );
{%- if interface == "tcp" %}
    let tcp = TcpServer::new(
        stack,
        PORT,
        RX.init([0; SOCKET_BUFFER]),
        TX.init([0; SOCKET_BUFFER]),
    );
    Net { runner, tcp }
{%- else %}
    let udp = UdpLink::new(
        stack,
        PORT,
        RX_META.init([PacketMetadata::EMPTY; 4]),
        RX.init([0; SOCKET_BUFFER]),
        TX_META.init([PacketMetadata::EMPTY; 4]),
        TX.init([0; SOCKET_BUFFER]),
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
