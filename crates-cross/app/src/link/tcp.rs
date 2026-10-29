//! Транспорт протокола поверх TCP — сокет на стеке платы (`board.net.stack`).
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "tcp" %}
{%- if f[3] == "modbus" or f[3] == "protobuf" %}
//! {% if f[3] == "modbus" %}Заголовок MBAP{% else %}Кадры COBS + CRC{% endif %}, канал узла `{% if f[3] == "modbus" %}MODBUS{% else %}PROTOBUF{% endif %}_{{ f[4] | upcase }}`.

use domain::net::Stack;

/// Порт, на котором протокол ждёт хост: {% if f[3] == "modbus" %}502 — стандартный порт Modbus
/// TCP и UDP{% else %}5000 — выбран шаблоном, у protobuf стандартного нет{% endif %}. Порт задаёт протокол, а не
/// плата.
const PORT: u16 = {% if f[3] == "modbus" %}502{% else %}5000{% endif %};
{%- if f[3] == "protobuf" %}

/// Буфер канала — сообщение и CRC (`rx_len`).
pub const BUFFER: usize = ::adapters::cobs::rx_len(::domain::protobuf::REQUEST_MAX);
/// Кадр передачи — худший случай COBS от всего буфера канала (`tx_len`).
pub const FRAME: usize = ::adapters::cobs::tx_len(BUFFER);
{%- endif %}

/// Канал узла `{% if f[3] == "modbus" %}MODBUS{% else %}PROTOBUF{% endif %}_{{ f[4] | upcase }}` над стеком платы (`board.net.stack`).
{%- if f[3] == "modbus" %}
pub type Link = ::adapters::modbus::Tcp<::adapters::net::TcpServer>;
{%- else %}
pub type Link = ::adapters::cobs::Cobs<'static, ::adapters::net::TcpServer>;
{%- endif %}

/// Канал над стеком `stack` в памяти, которую объявляет граф: буферы сокета
/// `rx`/`tx` (`crate::link::SOCKET_BUFFER`){% if f[3] == "protobuf" %}, буфер канала `buf` ([`BUFFER`]) и
/// кадра передачи `frame` ([`FRAME`]){% endif %}.
{%- if f[3] == "modbus" %}
pub fn link(stack: Stack<'static>, rx: &'static mut [u8], tx: &'static mut [u8]) -> Link {
    ::adapters::modbus::Tcp::new(::adapters::net::TcpServer::new(stack, PORT, rx, tx))
}
{%- else %}
pub fn link(
    stack: Stack<'static>,
    rx: &'static mut [u8],
    tx: &'static mut [u8],
    buf: &'static mut [u8],
    frame: &'static mut [u8],
) -> Link {
    ::adapters::cobs::Cobs::new(
        ::adapters::net::TcpServer::new(stack, PORT, rx, tx),
        buf,
        frame,
    )
}
{%- endif %}
{%- endif %}
{%- endif %}
{%- endfor %}
