#![doc = include_str!("../../../docs/modules/app-graph.md")]
// Разворачивает этот файл `supervisor_graph!`, и разворачивает он его в
// `crates-cross/app`, то есть в бинарник. `pub fn` в приватном модуле бинарника
// для `dead_code` мёртв, если его никто не зовёт, а звать некому именно то,
// ради чего макрос их и эмитит: получатели `publish:`, `provide_*` и
// `*_take()` — это ручки к полям графа, за которые отвечает проект. В сыром
// шаблоне подписчиков нет, и без этого `-D warnings` в `cargo xtask lint cross`
// ронял бы сборку на месте, где не сломалось ничего. Свою логику сюда не пишут
// — она в `domain`.
#![allow(
    dead_code,
    reason = "граф генерируется целиком: часть функций без читателей, пока нет подписчиков"
)]

{%- if graph == "true" %}
use defmt::{info, warn};
{%- if protocol == "protobuf" or interface == "tcp" or interface == "udp" %}
use static_cell::ConstStaticCell;
{%- endif %}
use supervisor::{Duration, Liveness, supervisor_graph};
{%- if protocol == "modbus" %}

/// Транспорт узла `MODBUS` поверх {% if interface == "uart" %}UART{% elsif interface == "tcp" %}TCP{% elsif interface == "usb" %}USB CDC{% else %}UDP{% endif %} платы: {% if interface == "uart" or interface == "usb" %}кадр RTU с CRC{% else %}заголовок MBAP{% endif %}.
type ModbusLink = ::adapters::modbus::{% if interface == "uart" %}Rtu<bsp::serial::Serial>{% elsif interface == "usb" %}Rtu<::adapters::usb::CdcStream<'static, bsp::usb::UsbDriver>>{% elsif interface == "tcp" %}Tcp<::adapters::net::TcpServer>{% else %}Udp<::adapters::net::UdpLink>{% endif %};
{%- endif %}
{%- if protocol == "protobuf" %}

/// Канал узла `HOST` поверх {% if interface == "uart" %}UART{% elsif interface == "tcp" %}TCP{% elsif interface == "usb" %}USB CDC{% else %}UDP{% endif %} платы: {% if interface == "udp" %}датаграмма — уже кадр{% else %}кадры COBS + CRC{% endif %}.
{%- if interface == "usb" %}
type HostLink =
    ::adapters::cobs::Cobs<'static, ::adapters::usb::CdcStream<'static, bsp::usb::UsbDriver>>;
{%- else %}
type HostLink = {% if interface == "udp" %}::adapters::net::UdpLink{% else %}::adapters::cobs::Cobs<'static, {% if interface == "uart" %}bsp::serial::Serial{% else %}::adapters::net::TcpServer{% endif %}>{% endif %};
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

{%- if interface == "tcp" or interface == "udp" %}

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

/// Как часто тикер графа опрашивает сторожей узлов и кормит железо.
///
/// Единственная цифра графа, которая живёт здесь: она про один аппаратный
/// сторож на всех, а не про какой-то узел. Таймаут узла — во фрагменте
/// (`domain::heartbeat::WATCHDOG`), аппаратный — у железа
/// (`bsp::wdg::HW_TIMEOUT_US`), и соотношение между ними проверяет
/// `const`-assert ниже. Компилятор проверяет сам `check_every` против
/// аппаратного таймаута — `supervisor_graph!` эмитит `const`-assert, читая
/// `HARDWARE_TIMEOUT` у типа сторожа; левое звено цепочки (период отметок <
/// таймаут узла) держит `const`-assert во фрагменте (`domain::heartbeat`) —
/// `docs/watchdog.md`.
const WATCHDOG_CHECK_EVERY: Duration = Duration::from_millis(100);

// Правое звено цепочки таймаутов, и единственное место в шаблоне, где видны оба
// его конца: таймаут узла живёт в `domain`, аппаратный — в `bsp`. `supervisor`
// намеренно не связывает дедлайн узла с таймаутом железа (граф с длинным
// дедлайном корректен сам по себе), а шаблон — должен: иначе `check_every`
// укладывается, все гейты зелёные, а IWDG сбрасывает МК раньше, чем граф
// заметит застой узла, и сброс приходит от железа — ни один узел не назван причиной, а
// диагностика `supervisor` оказывается мёртвым кодом.
//
// Узлов с `watchdog:` в списке `fragments:` ниже ровно два; новый фрагмент со
// своим `watchdog:` обязан встать в этот assert — иначе его дедлайн проверит
// один только этот комментарий. `<` на `Duration` в `const` не работает (нет
// const-трейтов), поэтому сравниваются микросекунды — как в `HW_TIMEOUT_US`,
// а не тики, чья частота зависит от фичи `tick-hz-*`.
const _: () = assert!(
    ::domain::heartbeat::WATCHDOG.as_micros() + WATCHDOG_CHECK_EVERY.as_micros()
        < bsp::wdg::HW_TIMEOUT_US as u64
        && ::domain::cpu_load::WATCHDOG.as_micros() + WATCHDOG_CHECK_EVERY.as_micros()
            < bsp::wdg::HW_TIMEOUT_US as u64,
    "таймаут узла плюс WATCHDOG_CHECK_EVERY обязан быть меньше bsp::wdg::HW_TIMEOUT_US: иначе железо \
     сбросит МК раньше, чем узел заметит зависшую задачу, и сброс придёт от железа, а не от supervisor"
);

supervisor_graph! {
    // Узлы приезжают из `domain`: каждая подсистема объявляет свой фрагмент
    // (`supervisor_fragment!`) рядом со своей задачей — вместе с политикой
    // перезапуска и таймаутом, — а здесь остаётся то, что знает про железо:
    // `boot:`-объект, блок `watchdog:` и список фрагментов. Растёт проект —
    // растёт список, а не тело графа.
    //
    // Порядок items фиксированный: `boot:` → `watchdog:` → `fragments:`
    // последним. Имя узлу даёт запись списка (`as HEARTBEAT`), не фрагмент: тот
    // же фрагмент под двумя `as` — два узла. Всё остальное, что объявляет
    // фрагмент (слоты, `provide_*`), получает префикс этого имени.
    boot: board: bsp::Board;

    watchdog: bsp::wdg::BoardWatchdog = board.watchdog.arm(),
        check_every: WATCHDOG_CHECK_EVERY, observe: report_liveness;

    fragments: [
        ::domain::HEARTBEAT_FRAG as HEARTBEAT,

        // Узел `CPU_LOAD` меряет загрузку процессора. Вход — счётчик тиков сна,
        // который ведёт ручной цикл исполнителя в `main.rs` (docs/modules/
        // app-cpu-load.md); всё, что нужно фрагменту, — ссылка на него: тип
        // входа объявляет подсистема (`domain::cpu_load::IdleMeter`), а
        // значение собирает приложение.
        //
        // Подписчика в шаблоне нет: слот `PERCENT` объявлен явно, единицей, и
        // первым его занимает тот, кто первым позовёт
        // `cpu_load_percent_receiver()`. Само значение в лог не идёт — читатель
        // должен быть, иначе процент некуда девать.
        ::domain::CPU_LOAD_FRAG as CPU_LOAD = ::domain::cpu_load::IdleMeter {
            sleep_ticks: crate::cpu_load::sleep_ticks(),
        },
{%- if ota == "true" %}

        // Фрагмент OTA — с параметрами и своей привязкой. Параметры
        // (`<Link, Updater>`) — типы двух его слотов: слот — `static`, тип ему
        // нужен, а знает его только эта сторона; подставили свой транспорт
        // вместо заглушки — меняйте первый аргумент. Привязка (`= …`) — вход
        // узла (`domain::ota::OtaWiring`): канал `link()` и адаптер из разделов
        // платы (`board.partitions`, `updater()` в `main.rs`) и режим
        // применения (`Mode::{% if signed == "true" %}Signed` с версией этого образа
        // из `crate::image`{% else %}Plain`{% endif %}). Алгоритм обновления — не железо, поэтому
        // собирает его `app`, а `bsp` отдаёт только разделы. Вычисляется
        // привязка в прологе `spawn_all` первой строкой — раньше инициализатора
        // блока `watchdog:`, как бы они ни стояли в тексте, — частичным move:
        // `partitions` уезжает в слот узла, `watchdog` следом забирает сторож,
        // остаток `board` дропается в конце пролога.
        ::domain::OTA_FRAG<crate::Link, crate::Updater> as OTA = ::domain::ota::OtaWiring {
            link: crate::link(),
            flash: crate::updater(board.partitions),
{%- if signed == "true" %}
            mode: ::domain::ota::Mode::Signed {
                running_version: crate::image::FW_VERSION,
            },
{%- else %}
            mode: ::domain::ota::Mode::Plain,
{%- endif %}
        },
{%- endif %}
{%- if interface == "usb" %}

        // Узел `USB` — фоновая работа устройства `embassy-usb` (`domain::usb`) над
        // драйвером платы `bsp::usb::UsbDriver`: без него хост не видит
        // устройства, а поток CDC — ни одного пакета.
        ::domain::USB_FRAG<::domain::usb::UsbDevice<'static, bsp::usb::UsbDriver>> as USB =
            ::domain::usb::UsbWiring { device: board.usb.device },
{%- endif %}
{%- if interface == "tcp" or interface == "udp" %}

        // Узел `NET` — фоновая работа стека `embassy-net` (`domain::net`) над
        // устройством платы `bsp::net::Device`: без него сокеты протокола не
        // получают ни одного пакета.
        ::domain::NET_FRAG<::domain::net::Runner<'static, bsp::net::Device>> as NET =
            ::domain::net::NetWiring { runner: board.net.runner },
{%- endif %}
{%- if protocol == "modbus" %}

        // Узел `MODBUS` — Modbus slave поверх {% if interface == "uart" %}UART{% elsif interface == "tcp" %}TCP{% elsif interface == "usb" %}USB CDC{% else %}UDP{% endif %} платы: транспорт `ModbusLink`
        // вокруг `{% if interface == "uart" %}board.serial{% elsif interface == "tcp" %}socket(board.net.stack){% elsif interface == "usb" %}board.usb.cdc{% else %}socket(board.net.stack){% endif %}` ({% if interface == "uart" %}заглушка, пока проект не подставит свой UART —
        // docs/modules/bsp-serial.md{% elsif interface == "usb" %}поток CDC над заглушкой драйвера, пока проект не
        // подставит свой USB — docs/modules/bsp-usb.md{% else %}сокет стека над заглушкой устройства, пока проект не подставит
        // Ethernet — docs/modules/bsp-net.md{% endif %}) и карта регистров
        // `domain::modbus::EmptyRegisters` — пустая, её проект описывает в `domain`.
        // Адрес slave'а — `unit`{% if interface == "tcp" or interface == "udp" %}: по сети устройство адресуется по IP, и slave
        // отвечает на любой адрес в MBAP{% endif %}.
        ::domain::MODBUS_SLAVE_FRAG<ModbusLink, ::domain::modbus::EmptyRegisters> as MODBUS =
            ::domain::modbus::ModbusSlaveWiring {
                link: ModbusLink::new({% if interface == "uart" %}board.serial{% elsif interface == "tcp" %}socket(board.net.stack){% elsif interface == "usb" %}board.usb.cdc{% else %}socket(board.net.stack){% endif %}),
                regs: ::domain::modbus::EmptyRegisters,
                unit: 1,
            },
{%- elsif protocol == "canopen" %}

        // Узел `CANOPEN` — узел CANopen (стек `zencan`) на шине CAN платы:
        // `board.can` (заглушка, пока проект не подставит свой контроллер —
        // docs/modules/bsp-can.md) и словарь объектов из `crates-host/canopen`
        // (`device.toml`). Адрес узла в сети — `node_id`, 1..=127.{% if config == "true" %} Хранилище —
        // `board.settings`: в нём узел сохраняет объекты по команде `save` и адрес,
        // назначенный по LSS (ключи `domain::canopen::{OBJECTS_KEY, NODE_ID_KEY}`).{% else %} Раздела
        // настроек нет — объекты и адрес не сохраняются.{% endif %}
        ::domain::CANOPEN_FRAG<bsp::can::Can, {% if config == "true" %}bsp::settings::Settings{% else %}::adapters::settings::Unavailable{% endif %}> as CANOPEN =
            ::domain::canopen::CanOpenWiring {
                bus: board.can,
                node_id: 1,
                dictionary: ::domain::canopen::dictionary(),
                settings: {% if config == "true" %}board.settings{% else %}::adapters::settings::Unavailable{% endif %},
                persist: {{config}},
            },
{%- elsif protocol == "protobuf" %}

        // Узел `HOST` — сообщения protobuf от хоста поверх {% if interface == "uart" %}UART{% elsif interface == "tcp" %}TCP{% elsif interface == "usb" %}USB CDC{% else %}UDP{% endif %} платы: канал
        // `HostLink` вокруг `{% if interface == "uart" %}board.serial{% elsif interface == "tcp" %}socket(board.net.stack){% elsif interface == "usb" %}board.usb.cdc{% else %}socket(board.net.stack){% endif %}` ({% if interface == "uart" %}заглушка, пока проект не подставит свой
        // UART — docs/modules/bsp-serial.md{% elsif interface == "usb" %}поток CDC над заглушкой драйвера, пока
        // проект не подставит свой USB — docs/modules/bsp-usb.md{% else %}сокет стека над заглушкой устройства, пока проект не
        // подставит Ethernet — docs/modules/bsp-net.md{% endif %}) и обработчик `domain::host::Service` над
        // схемой `crates-host/proto`. Буферы — статики `HOST_*` выше.
        ::domain::HOST_FRAG<HostLink, ::domain::host::Service> as HOST = ::domain::host::HostWiring {
{%- if interface == "udp" %}
            link: socket(board.net.stack),
{%- else %}
            link: HostLink::new({% if interface == "uart" %}board.serial{% elsif interface == "tcp" %}socket(board.net.stack){% elsif interface == "usb" %}board.usb.cdc{% else %}socket(board.net.stack){% endif %}, HOST_FRAME.take()),
{%- endif %}
            service: ::domain::host::Service::new(crate::build::PKG_VERSION),
            rx: HOST_RX.take(),
            tx: HOST_TX.take(),
        },
{%- endif %}
    ];
}

/// Куда уходит просрочка наблюдаемого узла.
///
/// Наблюдатель — часть декларации графа (`observe:` в блоке `watchdog:`), а не
/// то, что приложение устанавливает вызовом: узел с маркером `observe` и без
/// этой клаузы — ошибка компиляции, и клауза без единого такого узла тоже.
/// Каждая половина по отдельности выглядит рабочей и не является — узел,
/// докладывающий в никуда, тише ненаблюдаемого, но не безопаснее.
///
/// Печатью список действий не исчерпывается, он с неё начинается: что делать
/// с зависшим узлом — перезапустить соседа, деградировать, поднять тревогу —
/// это доменное знание, которого у сторожа нет.
fn report_liveness(node: &'static str, liveness: Liveness) {
    match liveness {
        Liveness::Stale => warn!("app: узел {} перестал отмечаться", node),
        Liveness::Recovered => info!("app: узел {} отметился снова", node),
    }
}

{%- endif %}
