//! Транспорт протокола поверх UDP — сокет на стеке платы (`board.net.stack`).
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "udp" %}
{%- if f[3] == "modbus" or f[3] == "protobuf" %}
//! Датаграмма — уже кадр, канал узла `{% if f[3] == "modbus" %}MODBUS{% else %}PROTOBUF{% endif %}_{{ f[4] | upcase }}`.

use core::mem::MaybeUninit;

use domain::net::Stack;
use embassy_net::udp::PacketMetadata;

/// Порт, на котором протокол ждёт хост: {% if f[3] == "modbus" %}502 — стандартный порт Modbus
/// TCP и UDP{% else %}5000 — выбран шаблоном, у protobuf стандартного нет{% endif %}. Порт задаёт протокол, а не
/// плата.
const PORT: u16 = {% if f[3] == "modbus" %}502{% else %}5000{% endif %};

/// Буфер канала — {% if f[3] == "modbus" %}самая длинная датаграмма Modbus{% else %}запрос{% endif %}.
pub const BUFFER: usize = {% if f[3] == "modbus" %}::adapters::modbus::DATAGRAM{% else %}::domain::protobuf::REQUEST_MAX{% endif %};

/// Сколько датаграмм держит очередь сокета в каждую сторону.
const QUEUE: usize = 4;

/// Метаданные датаграмм сокета — по записи на датаграмму в очереди. Нулевые
/// байты — не `PacketMetadata`, поэтому память под них — `MaybeUninit`, а
/// заполняет её [`link`].
pub type Meta = MaybeUninit<[PacketMetadata; QUEUE]>;

/// Канал узла `{% if f[3] == "modbus" %}MODBUS{% else %}PROTOBUF{% endif %}_{{ f[4] | upcase }}` над стеком платы (`board.net.stack`).
{%- if f[3] == "modbus" %}
pub type Link = ::adapters::modbus::Udp<::adapters::net::UdpLink>;
{%- else %}
pub type Link = ::adapters::net::UdpLink;
{%- endif %}

/// Канал над стеком `stack` в памяти, которую объявляет граф: метаданные
/// ([`Meta`]) и буферы сокета (`crate::link::SOCKET_BUFFER`), буфер канала
/// `frame` ([`BUFFER`]).
pub fn link(
    stack: Stack<'static>,
    rx_meta: &'static mut Meta,
    rx: &'static mut [u8],
    tx_meta: &'static mut Meta,
    tx: &'static mut [u8],
    frame: &'static mut [u8],
) -> Link {
    let rx_meta = rx_meta.write([PacketMetadata::EMPTY; QUEUE]);
    let tx_meta = tx_meta.write([PacketMetadata::EMPTY; QUEUE]);
{%- if f[3] == "modbus" %}
    ::adapters::modbus::Udp::new(::adapters::net::UdpLink::new(
        stack, PORT, rx_meta, rx, tx_meta, tx, frame,
    ))
{%- else %}
    ::adapters::net::UdpLink::new(stack, PORT, rx_meta, rx, tx_meta, tx, frame)
{%- endif %}
}
{%- endif %}
{%- endif %}
{%- endfor %}
