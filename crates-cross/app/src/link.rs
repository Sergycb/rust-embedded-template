//! Транспорты протоколов: подмодуль на транспорт с протоколом, с именем
//! транспорта (`crate::link::usart1`, `crate::link::tcp`).
//!
//! Здесь — тип канала (`Link`: какое кадрирование над каким рычагом платы,
//! выбор генерации) и его память — статики приложения, которые `buffers()`
//! отдаёт ссылками. Собирает канал сам узел протокола (`domain::server`, порт
//! `ports::Assemble`) из рычага платы и этих буферов. Своё кадрирование —
//! другой тип `Link`; свой размер буфера или порт — правьте статики здесь.
{%- assign xs = links | split: "," %}
{%- if has_protobuf == "true" %}

/// Самое длинное сообщение схемы, под которое заведены буферы каналов узлов
/// `HOST_*`. Схема с сообщениями длиннее — меняйте здесь; узел предупредит
/// при старте, если самый длинный ответ схемы не помещается.
const HOST_MESSAGE: usize = 256;

{%- endif %}
{%- if protocol_sockets != "0" %}

/// Буфер сокета протокола — с запасом на самый длинный кадр Modbus TCP
/// (260 байт) и сообщение protobuf (`HOST_MESSAGE`).
const SOCKET_BUFFER: usize = 1024;
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "modbus" or f[3] == "protobuf" %}

/// Транспорт `{{ f[4] }}`: {% if f[3] == "modbus" %}{% if f[0] == "uart" or f[0] == "usb" %}кадр RTU с CRC{% else %}заголовок MBAP{% endif %}{% else %}{% if f[0] == "udp" %}датаграмма — уже кадр{% else %}кадры COBS + CRC{% endif %}{% endif %} поверх {% if f[0] == "uart" %}UART `{{ f[2] }}`{% elsif f[0] == "tcp" %}TCP{% elsif f[0] == "usb" %}USB CDC `{{ f[2] }}`{% else %}UDP{% endif %} платы.
pub mod {{ f[4] }} {
{%- if f[3] == "protobuf" or f[0] == "tcp" or f[0] == "udp" %}
    use static_cell::ConstStaticCell;
{% endif %}
    /// Канал узла `{% if f[3] == "modbus" %}MODBUS{% else %}HOST{% endif %}_{{ f[4] | upcase }}`; собирается над {% if f[0] == "uart" %}UART платы (`board.{{ f[4] }}`){% elsif f[0] == "usb" %}потоком CDC платы (`board.{{ f[4] }}.cdc`){% else %}стеком платы (`board.net.stack`){% endif %}.
{%- if f[3] == "modbus" %}
    pub type Link = ::adapters::modbus::{% if f[0] == "uart" %}Rtu<bsp::serial::{{ f[4] }}::Serial>{% elsif f[0] == "usb" %}Rtu<bsp::usb::{{ f[4] }}::Cdc>{% elsif f[0] == "tcp" %}Tcp<::adapters::net::TcpServer>{% else %}Udp<::adapters::net::UdpLink>{% endif %};
{%- else %}
    pub type Link = {% if f[0] == "udp" %}::adapters::net::UdpLink{% elsif f[0] == "tcp" %}::adapters::cobs::TcpCobs{% else %}::adapters::cobs::Cobs<'static, {% if f[0] == "uart" %}bsp::serial::{{ f[4] }}::Serial{% else %}bsp::usb::{{ f[4] }}::Cdc{% endif %}>{% endif %};
{%- endif %}

{%- if graph != "true" %}

    /// Рычаг платы, над которым собирается канал, — тип по имени для задачи
    /// без графа (задачи embassy не generic).
    pub type Source = {% if f[0] == "uart" %}bsp::serial::{{ f[4] }}::Serial{% elsif f[0] == "usb" %}bsp::usb::{{ f[4] }}::Cdc{% else %}bsp::net::Stack{% endif %};
{%- endif %}
{%- if f[3] == "protobuf" %}

    // Буфер канала — сообщение{% if f[0] != "udp" %} и CRC (`rx_len`); кадру передачи — место под
    // худший случай COBS от всего буфера канала (`tx_len`){% endif %}. `ConstStaticCell`, а
    // не `StaticCell`: значения известны при компиляции и лежат в `.bss`, а не
    // собираются на стеке при старте.
{%- if f[0] == "udp" %}
    static HOST_BUFFER: ConstStaticCell<[u8; super::HOST_MESSAGE]> =
        ConstStaticCell::new([0; super::HOST_MESSAGE]);
{%- else %}
    const BUFFER: usize = ::adapters::cobs::rx_len(super::HOST_MESSAGE);
    static HOST_BUFFER: ConstStaticCell<[u8; BUFFER]> = ConstStaticCell::new([0; BUFFER]);
    static HOST_FRAME: ConstStaticCell<[u8; ::adapters::cobs::tx_len(BUFFER)]> =
        ConstStaticCell::new([0; ::adapters::cobs::tx_len(BUFFER)]);
{%- endif %}
{%- endif %}
{%- if f[0] == "tcp" or f[0] == "udp" %}

    /// Порт, на котором протокол ждёт хост: {% if f[3] == "modbus" %}502 — стандартный порт Modbus
    /// TCP и UDP{% else %}5000 — выбран шаблоном, у protobuf стандартного нет{% endif %}. Порт задаёт протокол, а не
    /// плата.
    const PORT: u16 = {% if f[3] == "modbus" %}502{% else %}5000{% endif %};

    // Память сокета протокола: он держит её `&'static mut`. `ConstStaticCell` —
    // значения известны при компиляции и лежат в `.bss`.
    static SOCKET_RX: ConstStaticCell<[u8; super::SOCKET_BUFFER]> =
        ConstStaticCell::new([0; super::SOCKET_BUFFER]);
    static SOCKET_TX: ConstStaticCell<[u8; super::SOCKET_BUFFER]> =
        ConstStaticCell::new([0; super::SOCKET_BUFFER]);
{%- if f[0] == "udp" %}
    static SOCKET_RX_META: ConstStaticCell<[embassy_net::udp::PacketMetadata; 4]> =
        ConstStaticCell::new([embassy_net::udp::PacketMetadata::EMPTY; 4]);
    static SOCKET_TX_META: ConstStaticCell<[embassy_net::udp::PacketMetadata; 4]> =
        ConstStaticCell::new([embassy_net::udp::PacketMetadata::EMPTY; 4]);
{%- if f[3] == "modbus" %}
    // Буфер канала — самая длинная датаграмма Modbus.
    static MODBUS_BUFFER: ConstStaticCell<[u8; ::adapters::modbus::DATAGRAM]> =
        ConstStaticCell::new([0; ::adapters::modbus::DATAGRAM]);
{%- endif %}
{%- endif %}
{%- endif %}

    /// Память канала — ссылки на статики выше. Статики берутся ровно один раз:
    /// узел собирается один раз за запуск.
{%- if f[3] == "modbus" and f[0] != "tcp" and f[0] != "udp" %}
    pub fn buffers() {}
{%- elsif f[0] == "tcp" and f[3] == "modbus" %}
    pub fn buffers() -> ::adapters::net::TcpServerBuffers {
        ::adapters::net::TcpServerBuffers {
            port: PORT,
            rx: SOCKET_RX.take(),
            tx: SOCKET_TX.take(),
        }
    }
{%- elsif f[0] == "udp" %}
    pub fn buffers() -> ::adapters::net::UdpBuffers {
        ::adapters::net::UdpBuffers {
            port: PORT,
            rx_meta: SOCKET_RX_META.take(),
            rx: SOCKET_RX.take(),
            tx_meta: SOCKET_TX_META.take(),
            tx: SOCKET_TX.take(),
            frame: {% if f[3] == "modbus" %}MODBUS_BUFFER{% else %}HOST_BUFFER{% endif %}.take(),
        }
    }
{%- elsif f[0] == "tcp" %}
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
{%- else %}
    pub fn buffers() -> ::adapters::cobs::CobsBuffers {
        ::adapters::cobs::CobsBuffers {
            buf: HOST_BUFFER.take(),
            frame: HOST_FRAME.take(),
        }
    }
{%- endif %}
}
{%- endif %}
{%- endfor %}
