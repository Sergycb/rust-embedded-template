//! Транспорт протокола поверх UDP — сокет на стеке платы (`board.net.stack`).
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "udp" %}
{%- if f[3] == "modbus" or f[3] == "protobuf" %}
//! Датаграмма — уже кадр, канал узла `{% if f[3] == "modbus" %}MODBUS{% else %}HOST{% endif %}_{{ f[4] | upcase }}`.

use embassy_net::udp::PacketMetadata;
use static_cell::ConstStaticCell;

/// Порт, на котором протокол ждёт хост: {% if f[3] == "modbus" %}502 — стандартный порт Modbus
/// TCP и UDP{% else %}5000 — выбран шаблоном, у protobuf стандартного нет{% endif %}. Порт задаёт протокол, а не
/// плата.
const PORT: u16 = {% if f[3] == "modbus" %}502{% else %}5000{% endif %};

/// Канал узла `{% if f[3] == "modbus" %}MODBUS{% else %}HOST{% endif %}_{{ f[4] | upcase }}`; собирается над стеком платы (`board.net.stack`).
{%- if f[3] == "modbus" %}
pub type Link = ::adapters::modbus::Udp<::adapters::net::UdpLink>;
{%- else %}
pub type Link = ::adapters::net::UdpLink;
{%- endif %}
{%- if graph != "true" %}

/// Рычаг платы, над которым собирается канал, — тип по имени для задачи
/// без графа (задачи embassy не generic).
pub type Source = bsp::net::Stack;
{%- endif %}

// Память сокета протокола: он держит её `&'static mut`. `ConstStaticCell` —
// значения известны при компиляции и лежат в `.bss`.
static SOCKET_RX: ConstStaticCell<[u8; crate::link::SOCKET_BUFFER]> =
    ConstStaticCell::new([0; crate::link::SOCKET_BUFFER]);
static SOCKET_TX: ConstStaticCell<[u8; crate::link::SOCKET_BUFFER]> =
    ConstStaticCell::new([0; crate::link::SOCKET_BUFFER]);
static SOCKET_RX_META: ConstStaticCell<[PacketMetadata; 4]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 4]);
static SOCKET_TX_META: ConstStaticCell<[PacketMetadata; 4]> =
    ConstStaticCell::new([PacketMetadata::EMPTY; 4]);
{%- if f[3] == "modbus" %}
// Буфер канала — самая длинная датаграмма Modbus.
static FRAME: ConstStaticCell<[u8; ::adapters::modbus::DATAGRAM]> =
    ConstStaticCell::new([0; ::adapters::modbus::DATAGRAM]);
{%- else %}
// Буфер канала — сообщение.
static FRAME: ConstStaticCell<[u8; crate::link::HOST_MESSAGE]> =
    ConstStaticCell::new([0; crate::link::HOST_MESSAGE]);
{%- endif %}

/// Память канала — ссылки на статики выше. Статики берутся ровно один раз:
/// узел собирается один раз за запуск.
pub fn buffers() -> ::adapters::net::UdpBuffers {
    ::adapters::net::UdpBuffers {
        port: PORT,
        rx_meta: SOCKET_RX_META.take(),
        rx: SOCKET_RX.take(),
        tx_meta: SOCKET_TX_META.take(),
        tx: SOCKET_TX.take(),
        frame: FRAME.take(),
    }
}
{%- endif %}
{%- endif %}
{%- endfor %}
