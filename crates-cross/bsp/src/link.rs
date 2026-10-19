//! Транспорт протокола, готовый к работе: {% if protocol == "modbus" %}{% if interface == "uart" or interface == "usb" %}кадр RTU с CRC{% else %}заголовок MBAP{% endif %}{% else %}{% if interface == "udp" %}датаграмма — уже кадр{% else %}кадры COBS + CRC{% endif %}{% endif %} поверх {% if interface == "uart" %}UART{% elsif interface == "tcp" %}TCP{% elsif interface == "usb" %}USB CDC{% else %}UDP{% endif %} платы.
//!
//! `Board::link` — этот объект: буферы и сокет уже заведены здесь, узлу
//! протокола остаётся принять его (`Board::link` → вход узла в `graph.rs`).
//! Протокол выбран при генерации, поэтому и транспорт один; своё кадрирование —
//! правьте этот файл.

{%- if protocol == "protobuf" or protocol_socket == "true" %}
use static_cell::ConstStaticCell;
{%- endif %}

/// Транспорт узла {% if protocol == "modbus" %}`MODBUS`{% else %}`HOST`{% endif %}.
{%- if protocol == "modbus" %}
{%- if interface == "usb" %}
pub type Link = ::adapters::modbus::Rtu<crate::usb::Cdc>;
{%- else %}
pub type Link = ::adapters::modbus::{% if interface == "uart" %}Rtu<crate::serial::Serial>{% elsif interface == "tcp" %}Tcp<::adapters::net::TcpServer>{% else %}Udp<::adapters::net::UdpLink>{% endif %};
{%- endif %}
{%- else %}
{%- if interface == "usb" %}
pub type Link = ::adapters::cobs::Cobs<'static, crate::usb::Cdc>;
{%- else %}
pub type Link = {% if interface == "udp" %}::adapters::net::UdpLink{% else %}::adapters::cobs::Cobs<'static, {% if interface == "uart" %}crate::serial::Serial{% else %}::adapters::net::TcpServer{% endif %}>{% endif %};
{%- endif %}
{%- endif %}

/// Что плата отдаёт под транспорт: {% if interface == "uart" %}UART (заглушка, пока проект не подставит свой —
/// docs/modules/bsp-serial.md){% elsif interface == "usb" %}поток CDC (над заглушкой драйвера, пока проект не подставит
/// свой USB — docs/modules/bsp-usb.md){% else %}стек `embassy-net`, на котором открывается сокет (над заглушкой
/// устройства, пока проект не подставит Ethernet — docs/modules/bsp-net.md){% endif %}.
pub(crate) type Source = {% if interface == "uart" %}crate::serial::Serial{% elsif interface == "usb" %}crate::usb::Cdc{% else %}crate::net::Stack{% endif %};
{%- if protocol == "protobuf" %}

/// Самое длинное сообщение схемы, под которое заведены буферы узла `HOST`.
/// Схема с сообщениями длиннее — меняйте здесь; узел предупредит при старте,
/// если самый длинный ответ схемы не помещается.
const HOST_MESSAGE: usize = 256;

// Буферы узла `HOST`{% if interface != "udp" %}: приёму нужно место ещё под CRC (`rx_len`), кадру
// передачи — под худший случай COBS (`tx_len`){% endif %}. `ConstStaticCell`, а не
// `StaticCell`: значения известны при компиляции и лежат в `.bss`, а не
// собираются на стеке при старте.
{%- if interface == "udp" %}
static HOST_RX: ConstStaticCell<[u8; HOST_MESSAGE]> = ConstStaticCell::new([0; HOST_MESSAGE]);
{%- else %}
static HOST_FRAME: ConstStaticCell<[u8; ::adapters::cobs::tx_len(HOST_MESSAGE)]> =
    ConstStaticCell::new([0; ::adapters::cobs::tx_len(HOST_MESSAGE)]);
static HOST_RX: ConstStaticCell<[u8; ::adapters::cobs::rx_len(HOST_MESSAGE)]> =
    ConstStaticCell::new([0; ::adapters::cobs::rx_len(HOST_MESSAGE)]);
{%- endif %}
static HOST_TX: ConstStaticCell<[u8; HOST_MESSAGE]> = ConstStaticCell::new([0; HOST_MESSAGE]);

/// Буферы приёма и передачи сообщений узла `HOST` (`Board::host_buffers`).
#[derive(Debug)]
pub struct HostBuffers {
    /// Приём.
    pub rx: &'static mut [u8],
    /// Передача.
    pub tx: &'static mut [u8],
}

/// Забирает буферы узла `HOST`. Статики берутся ровно один раз — `Board::new`
/// зовётся один раз за запуск.
pub(crate) fn host_buffers() -> HostBuffers {
    HostBuffers {
        rx: HOST_RX.take(),
        tx: HOST_TX.take(),
    }
}
{%- endif %}
{%- if protocol_socket == "true" %}

/// Порт, на котором протокол ждёт хост: {% if protocol == "modbus" %}502 — стандартный порт Modbus
/// TCP и UDP{% else %}5000 — выбран шаблоном, у protobuf стандартного нет{% endif %}. Порт задаёт протокол, а не
/// плата, поэтому он здесь, а не в `bsp::net`.
const PORT: u16 = {% if protocol == "modbus" %}502{% else %}5000{% endif %};

/// Буфер сокета протокола — с запасом на самый длинный кадр Modbus TCP
/// (260 байт) и сообщение protobuf (`HOST_MESSAGE`).
const SOCKET_BUFFER: usize = 1024;

// Память сокета протокола: он держит её `&'static mut`. `ConstStaticCell` —
// значения известны при компиляции и лежат в `.bss`.
static SOCKET_RX: ConstStaticCell<[u8; SOCKET_BUFFER]> = ConstStaticCell::new([0; SOCKET_BUFFER]);
static SOCKET_TX: ConstStaticCell<[u8; SOCKET_BUFFER]> = ConstStaticCell::new([0; SOCKET_BUFFER]);
{%- if interface == "udp" %}
static SOCKET_RX_META: ConstStaticCell<[crate::net::PacketMetadata; 4]> =
    ConstStaticCell::new([crate::net::PacketMetadata::EMPTY; 4]);
static SOCKET_TX_META: ConstStaticCell<[crate::net::PacketMetadata; 4]> =
    ConstStaticCell::new([crate::net::PacketMetadata::EMPTY; 4]);
{%- endif %}

/// Сокет протокола на стеке платы: {% if interface == "tcp" %}TCP-сервер — поток байтов под транспортом{% else %}UDP-канал — датаграммы{% endif %} протокола.
{%- if interface == "tcp" %}
fn socket(stack: crate::net::Stack) -> ::adapters::net::TcpServer {
    ::adapters::net::TcpServer::new(stack, PORT, SOCKET_RX.take(), SOCKET_TX.take())
}
{%- else %}
fn socket(stack: crate::net::Stack) -> ::adapters::net::UdpLink {
    ::adapters::net::UdpLink::new(
        stack,
        PORT,
        SOCKET_RX_META.take(),
        SOCKET_RX.take(),
        SOCKET_TX_META.take(),
        SOCKET_TX.take(),
    )
}
{%- endif %}
{%- endif %}

/// Собирает транспорт протокола вокруг `source`.
pub(crate) fn new(source: Source) -> Link {
{%- if protocol == "modbus" %}
    Link::new({% if interface == "tcp" or interface == "udp" %}socket(source){% else %}source{% endif %})
{%- elsif interface == "udp" %}
    socket(source)
{%- else %}
    Link::new({% if interface == "tcp" %}socket(source){% else %}source{% endif %}, HOST_FRAME.take())
{%- endif %}
}
