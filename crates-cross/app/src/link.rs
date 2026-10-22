//! Транспорты протоколов: подмодуль на транспорт с протоколом, с именем
//! транспорта (`crate::link::usart1`, `crate::link::tcp`).
//!
//! Плата отдаёт сырьё — поток UART, поток CDC, сетевой стек (`bsp::Board`), —
//! а кадрирование, буферы и сокет заводятся здесь: их размер и порт — решение
//! приложения, а не платы. Граф (`graph.rs`) или задача без графа берёт
//! транспорт `new(...)` — буфер сообщения у него внутри. Транспорты выбраны
//! при генерации; своё кадрирование — правьте этот файл.
{%- assign xs = links | split: "," %}
{%- if has_protobuf == "true" %}

/// Самое длинное сообщение схемы, под которое заведены буферы каналов узлов
/// `HOST_*`.
/// Схема с сообщениями длиннее — меняйте здесь; узел предупредит при старте,
/// если самый длинный ответ схемы не помещается.
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
    /// Транспорт узла `{% if f[3] == "modbus" %}MODBUS{% else %}HOST{% endif %}_{{ f[4] | upcase }}`.
{%- if f[3] == "modbus" %}
    pub type Link = ::adapters::modbus::{% if f[0] == "uart" %}Rtu<bsp::serial::{{ f[4] }}::Serial>{% elsif f[0] == "usb" %}Rtu<bsp::usb::{{ f[4] }}::Cdc>{% elsif f[0] == "tcp" %}Tcp<::adapters::net::TcpServer>{% else %}Udp<::adapters::net::UdpLink>{% endif %};
{%- else %}
    pub type Link = {% if f[0] == "udp" %}::adapters::net::UdpLink{% else %}::adapters::cobs::Cobs<'static, {% if f[0] == "uart" %}bsp::serial::{{ f[4] }}::Serial{% elsif f[0] == "usb" %}bsp::usb::{{ f[4] }}::Cdc{% else %}::adapters::net::TcpServer{% endif %}>{% endif %};
{%- endif %}

    /// Что плата отдаёт под транспорт: {% if f[0] == "uart" %}UART (заглушка, пока проект не подставит свой —
    /// docs/modules/bsp-serial.md){% elsif f[0] == "usb" %}поток CDC (над заглушкой драйвера, пока проект не подставит
    /// свой USB — docs/modules/bsp-usb.md){% else %}стек `embassy-net`, на котором открывается сокет (над заглушкой
    /// устройства, пока проект не подставит Ethernet — docs/modules/bsp-net.md){% endif %}.
    pub type Source = {% if f[0] == "uart" %}bsp::serial::{{ f[4] }}::Serial{% elsif f[0] == "usb" %}bsp::usb::{{ f[4] }}::Cdc{% else %}bsp::net::Stack{% endif %};
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

    /// Сокет протокола на стеке платы: {% if f[0] == "tcp" %}TCP-сервер — поток байтов под транспортом{% else %}UDP-канал — датаграммы{% endif %} протокола.
{%- if f[0] == "tcp" %}
    fn socket(stack: bsp::net::Stack) -> ::adapters::net::TcpServer {
        ::adapters::net::TcpServer::new(stack, PORT, SOCKET_RX.take(), SOCKET_TX.take())
    }
{%- else %}
    fn socket(stack: bsp::net::Stack) -> ::adapters::net::UdpLink {
        ::adapters::net::UdpLink::new(
            stack,
            PORT,
            SOCKET_RX_META.take(),
            SOCKET_RX.take(),
            SOCKET_TX_META.take(),
            SOCKET_TX.take(),
            {% if f[3] == "modbus" %}MODBUS_BUFFER{% else %}HOST_BUFFER{% endif %}.take(),
        )
    }
{%- endif %}
{%- endif %}

    /// Собирает транспорт протокола вокруг `source`.
    pub fn new(source: Source) -> Link {
{%- if f[3] == "modbus" %}
        Link::new({% if f[0] == "tcp" or f[0] == "udp" %}socket(source){% else %}source{% endif %})
{%- elsif f[0] == "udp" %}
        socket(source)
{%- else %}
        Link::new({% if f[0] == "tcp" %}socket(source){% else %}source{% endif %}, HOST_BUFFER.take(), HOST_FRAME.take())
{%- endif %}
    }
}
{%- endif %}
{%- endfor %}
