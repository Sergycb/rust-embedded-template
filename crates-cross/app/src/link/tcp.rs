//! Транспорт протокола поверх TCP — сокет на стеке платы (`board.net.stack`).
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "tcp" %}
{%- if f[3] == "modbus" or f[3] == "protobuf" %}
//! {% if f[3] == "modbus" %}Заголовок MBAP{% else %}Кадры COBS + CRC{% endif %}, канал узла `{% if f[3] == "modbus" %}MODBUS{% else %}HOST{% endif %}_{{ f[4] | upcase }}`.

use static_cell::ConstStaticCell;

/// Порт, на котором протокол ждёт хост: {% if f[3] == "modbus" %}502 — стандартный порт Modbus
/// TCP и UDP{% else %}5000 — выбран шаблоном, у protobuf стандартного нет{% endif %}. Порт задаёт протокол, а не
/// плата.
const PORT: u16 = {% if f[3] == "modbus" %}502{% else %}5000{% endif %};
{%- if f[3] == "protobuf" %}

// Буфер канала — сообщение и CRC (`rx_len`); кадру передачи — место под
// худший случай COBS от всего буфера канала (`tx_len`).
const BUFFER: usize = ::adapters::cobs::rx_len(crate::link::HOST_MESSAGE);
{%- endif %}

/// Канал узла `{% if f[3] == "modbus" %}MODBUS{% else %}HOST{% endif %}_{{ f[4] | upcase }}`; собирается над стеком платы (`board.net.stack`).
{%- if f[3] == "modbus" %}
pub type Link = ::adapters::modbus::Tcp<::adapters::net::TcpServer>;
{%- else %}
pub type Link = ::adapters::cobs::TcpCobs;
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
{%- if f[3] == "protobuf" %}
static HOST_BUFFER: ConstStaticCell<[u8; BUFFER]> = ConstStaticCell::new([0; BUFFER]);
static HOST_FRAME: ConstStaticCell<[u8; ::adapters::cobs::tx_len(BUFFER)]> =
    ConstStaticCell::new([0; ::adapters::cobs::tx_len(BUFFER)]);
{%- endif %}

/// Память канала — ссылки на статики выше. Статики берутся ровно один раз:
/// узел собирается один раз за запуск.
{%- if f[3] == "modbus" %}
pub fn buffers() -> ::adapters::net::TcpServerBuffers {
    ::adapters::net::TcpServerBuffers {
        port: PORT,
        rx: SOCKET_RX.take(),
        tx: SOCKET_TX.take(),
    }
}
{%- else %}
pub fn buffers() -> ::adapters::cobs::TcpCobsBuffers {
    ::adapters::cobs::TcpCobsBuffers {
        socket: ::adapters::net::TcpServerBuffers {
            port: PORT,
            rx: SOCKET_RX.take(),
            tx: SOCKET_TX.take(),
        },
        cobs: ::adapters::cobs::CobsBuffers {
            buf: HOST_BUFFER.take(),
            frame: HOST_FRAME.take(),
        },
    }
}
{%- endif %}
{%- endif %}
{%- endif %}
{%- endfor %}
