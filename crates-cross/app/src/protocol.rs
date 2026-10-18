//! {% if protocol != "none" %}Протокол связи проекта: транспорт вокруг интерфейса платы, буферы и вход
//! узла (`*_wiring`).
//!
//! Общее для обоих способов запустить узел: граф (`graph.rs`) подставляет вход
//! в `fragments:`, а проект без графа (`graph=no`) спавнит узел из `main`
//! (`spawn` ниже) — вход в обоих случаях собирает одна и та же функция.{% else %}Фоновые узлы транспорта проекта без графа (`graph=no`): `main` спавнит их
//! отсюда (`spawn` ниже). Протокола в проекте нет — он ваш, а транспорт и его
//! узел уже работают.{% endif %}
{%- if graph != "true" %}

use embassy_executor::Spawner;
{%- endif %}
{%- if protocol == "protobuf" or protocol_socket == "true" %}
use static_cell::ConstStaticCell;
{%- endif %}
{%- if protocol == "modbus" %}

/// Транспорт узла `MODBUS` поверх {% if interface == "uart" %}UART{% elsif interface == "tcp" %}TCP{% elsif interface == "usb" %}USB CDC{% else %}UDP{% endif %} платы: {% if interface == "uart" or interface == "usb" %}кадр RTU с CRC{% else %}заголовок MBAP{% endif %}.
{%- if interface == "usb" %}
pub type ModbusLink =
    ::adapters::modbus::Rtu<::adapters::usb::CdcStream<'static, bsp::usb::UsbDriver>>;
{%- else %}
pub type ModbusLink = ::adapters::modbus::{% if interface == "uart" %}Rtu<bsp::serial::Serial>{% elsif interface == "tcp" %}Tcp<::adapters::net::TcpServer>{% else %}Udp<::adapters::net::UdpLink>{% endif %};
{%- endif %}
{%- endif %}
{%- if protocol == "protobuf" %}

/// Канал узла `HOST` поверх {% if interface == "uart" %}UART{% elsif interface == "tcp" %}TCP{% elsif interface == "usb" %}USB CDC{% else %}UDP{% endif %} платы: {% if interface == "udp" %}датаграмма — уже кадр{% else %}кадры COBS + CRC{% endif %}.
{%- if interface == "usb" %}
pub type HostLink =
    ::adapters::cobs::Cobs<'static, ::adapters::usb::CdcStream<'static, bsp::usb::UsbDriver>>;
{%- else %}
pub type HostLink = {% if interface == "udp" %}::adapters::net::UdpLink{% else %}::adapters::cobs::Cobs<'static, {% if interface == "uart" %}bsp::serial::Serial{% else %}::adapters::net::TcpServer{% endif %}>{% endif %};
{%- endif %}

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
static SOCKET_RX_META: ConstStaticCell<[bsp::net::PacketMetadata; 4]> =
    ConstStaticCell::new([bsp::net::PacketMetadata::EMPTY; 4]);
static SOCKET_TX_META: ConstStaticCell<[bsp::net::PacketMetadata; 4]> =
    ConstStaticCell::new([bsp::net::PacketMetadata::EMPTY; 4]);
{%- endif %}

/// Сокет протокола на стеке платы: {% if interface == "tcp" %}TCP-сервер — поток байтов под транспортом{% else %}UDP-канал — датаграммы{% endif %} протокола.
{%- if interface == "tcp" %}
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
    )
}
{%- endif %}
{%- endif %}
{%- if protocol == "modbus" or protocol == "protobuf" %}

/// Что плата отдаёт под транспорт протокола: {% if interface == "uart" %}UART (заглушка, пока проект не подставит свой —
/// docs/modules/bsp-serial.md){% elsif interface == "usb" %}поток CDC (над заглушкой драйвера, пока проект не подставит
/// свой USB — docs/modules/bsp-usb.md){% else %}стек `embassy-net`, на котором открывается сокет (над заглушкой
/// устройства, пока проект не подставит Ethernet — docs/modules/bsp-net.md){% endif %}.
pub type Source = {% if interface == "uart" %}bsp::serial::Serial{% elsif interface == "usb" %}::adapters::usb::CdcStream<'static, bsp::usb::UsbDriver>{% else %}bsp::net::Stack{% endif %};
{%- endif %}
{%- if protocol == "modbus" %}

/// Вход узла `MODBUS`: Modbus slave поверх {% if interface == "uart" %}UART{% elsif interface == "tcp" %}TCP{% elsif interface == "usb" %}USB CDC{% else %}UDP{% endif %} платы — транспорт
/// [`ModbusLink`] вокруг `source` — и карта регистров
/// `domain::modbus::EmptyRegisters`: пустая, её проект описывает в `domain`.
/// Адрес slave'а — `unit`{% if interface == "tcp" or interface == "udp" %}: по сети устройство адресуется по IP, и slave
/// отвечает на любой адрес в MBAP{% endif %}.
pub fn modbus_wiring(
    source: Source,
) -> ::domain::modbus::ModbusSlaveWiring<ModbusLink, ::domain::modbus::EmptyRegisters> {
    ::domain::modbus::ModbusSlaveWiring {
        link: ModbusLink::new({% if interface == "tcp" or interface == "udp" %}socket(source){% else %}source{% endif %}),
        regs: ::domain::modbus::EmptyRegisters,
        unit: 1,
    }
}
{%- elsif protocol == "protobuf" %}

/// Вход узла `HOST`: сообщения protobuf от хоста поверх {% if interface == "uart" %}UART{% elsif interface == "tcp" %}TCP{% elsif interface == "usb" %}USB CDC{% else %}UDP{% endif %} платы — канал
/// [`HostLink`] вокруг `source` — и обработчик `domain::host::Service` над
/// схемой `crates-host/proto`. Буферы — статики `HOST_*` выше.
pub fn host_wiring(
    source: Source,
) -> ::domain::host::HostWiring<HostLink, ::domain::host::Service> {
    ::domain::host::HostWiring {
{%- if interface == "udp" %}
        link: socket(source),
{%- else %}
        link: HostLink::new({% if interface == "tcp" %}socket(source){% else %}source{% endif %}, HOST_FRAME.take()),
{%- endif %}
        service: ::domain::host::Service::new(crate::build::PKG_VERSION),
        rx: HOST_RX.take(),
        tx: HOST_TX.take(),
    }
}
{%- elsif protocol == "canopen" %}

/// Хранилище узла `CANOPEN`: {% if config == "true" %}раздел настроек платы — в нём узел сохраняет объекты по команде `save` и
/// адрес, назначенный по LSS (ключи `domain::canopen::{OBJECTS_KEY, NODE_ID_KEY}`){% else %}раздела настроек нет — объекты и адрес не сохраняются{% endif %}.
pub type Settings = {% if config == "true" %}bsp::settings::Settings{% else %}::adapters::settings::Unavailable{% endif %};

/// Вход узла `CANOPEN`: узел CANopen (стек `zencan`) на шине CAN платы — `bus`
/// (заглушка, пока проект не подставит свой контроллер — docs/modules/bsp-can.md) —
/// и словарь объектов из `crates-host/canopen` (`device.toml`). Адрес узла в
/// сети — `node_id`, 1..=127.
pub fn canopen_wiring(
    bus: bsp::can::Can,
    settings: Settings,
) -> ::domain::canopen::CanOpenWiring<bsp::can::Can, Settings> {
    ::domain::canopen::CanOpenWiring {
        bus,
        node_id: 1,
        dictionary: ::domain::canopen::dictionary(),
        settings,
        persist: {{config}},
    }
}
{%- endif %}
{%- if graph != "true" %}

// Задачи проекта без графа. `Spawner::spawn` берёт токен, а не функцию, а
// задача не может быть generic — поэтому каждая типизирована псевдонимами выше
// и `bsp`. Вход собирается внутри задачи: статики буферов берутся ровно один
// раз, а задача спавнится один раз.
{%- if interface == "tcp" or interface == "udp" %}

/// Узел `NET`: фоновая работа стека `embassy-net`.
#[embassy_executor::task]
async fn net_task(runner: ::domain::net::Runner<'static, bsp::net::Device>) {
    ::domain::net::task(runner).await;
}
{%- endif %}
{%- if interface == "usb" %}

/// Узел `USB`: фоновая работа устройства `embassy-usb`.
#[embassy_executor::task]
async fn usb_task(device: ::domain::usb::UsbDevice<'static, bsp::usb::UsbDriver>) {
    ::domain::usb::task(device).await;
}
{%- endif %}
{%- if protocol == "modbus" %}

/// Узел `MODBUS`.
#[embassy_executor::task]
async fn modbus_task(source: Source) {
    let w = modbus_wiring(source);
    ::domain::modbus::task(w.link, w.regs, w.unit).await;
}
{%- elsif protocol == "protobuf" %}

/// Узел `HOST`.
#[embassy_executor::task]
async fn host_task(source: Source) {
    let w = host_wiring(source);
    ::domain::host::task(w.link, w.service, w.rx, w.tx).await;
}
{%- elsif protocol == "canopen" %}

/// Узел `CANOPEN`.
#[embassy_executor::task]
async fn canopen_task(bus: bsp::can::Can, settings: Settings) {
    let w = canopen_wiring(bus, settings);
    ::domain::canopen::task(w.bus, w.node_id, w.dictionary, w.settings, w.persist).await;
}
{%- endif %}

/// Спавнит узлы {% if protocol != "none" %}протокола и транспорта{% else %}транспорта{% endif %}: то, что в проекте с графом делает `fragments:`.
/// Берёт объект платы, который лежит под {% if protocol != "none" %}протоколом{% else %}транспортом{% endif %}{% if interface == "tcp" or interface == "udp" %}, — сеть целиком: из неё
/// узел `NET` забирает `runner`, протокол — `stack`{% elsif interface == "usb" %}, — USB целиком: из него узел `USB`
/// забирает `device`, протокол — `cdc`{% endif %}.
pub fn spawn(spawner: Spawner, {% if interface == "uart" %}serial: bsp::serial::Serial{% elsif interface == "can" %}can: bsp::can::Can, settings: Settings{% elsif interface == "usb" %}usb: bsp::usb::Usb{% else %}net: bsp::net::Net{% endif %}) {
{%- if interface == "tcp" or interface == "udp" %}
    spawner.spawn(net_task(net.runner).expect("задача NET свежая: спавнится один раз"));
{%- endif %}
{%- if interface == "usb" %}
    spawner.spawn(usb_task(usb.device).expect("задача USB свежая: спавнится один раз"));
{%- endif %}
{%- if protocol == "modbus" %}
    spawner.spawn(modbus_task({% if interface == "uart" %}serial{% elsif interface == "usb" %}usb.cdc{% else %}net.stack{% endif %}).expect("задача MODBUS свежая: спавнится один раз"));
{%- elsif protocol == "protobuf" %}
    spawner.spawn(host_task({% if interface == "uart" %}serial{% elsif interface == "usb" %}usb.cdc{% else %}net.stack{% endif %}).expect("задача HOST свежая: спавнится один раз"));
{%- elsif protocol == "canopen" %}
    spawner.spawn(canopen_task(can, settings).expect("задача CANOPEN свежая: спавнится один раз"));
{%- endif %}
}
{%- endif %}
