#![doc = include_str!("../../../docs/modules/app-graph.md")]
// Разворачивает этот файл `supervisor_graph!`, и разворачивает он его в
// приватный модуль библиотеки `crates-cross/app` (наружу `lib.rs` отдаёт
// только `spawn_all`{% if ota == "true" %} и `Boot`{% endif %}). `pub fn` в приватном модуле
// для `dead_code` мёртв, если его никто не зовёт, а звать некому именно то,
// ради чего макрос их и эмитит: получатели `publish:`, `provide_*` и
// `*_take()` — это ручки к полям графа, за которые отвечает проект. В свежем
// проекте подписчиков нет, и без этого `-D warnings` в `cargo xtask lint cross`
// ронял бы сборку на месте, где не сломалось ничего. Свою логику сюда не пишут
// — она в `domain`.
#![allow(
    dead_code,
    reason = "граф генерируется целиком: часть функций без читателей, пока нет подписчиков"
)]

{%- if graph == "true" %}
{%- comment %} Плата — `boot:`-объект графа целиком, а с OTA — его поле `board`: рядом едут свойства образа (`Boot`). {% endcomment %}
{%- if ota == "true" %}{% assign board = "boot.board" %}{% else %}{% assign board = "board" %}{% endif %}
{%- capture info %}crate::device::info({{ board }}.spec{% if ota == "true" %}, boot.image{% endif %}){% endcapture %}
use defmt::{info, warn};
use supervisor::{Duration, Liveness, supervisor_graph};

supervisor_graph! {
    // Узлы приезжают из `domain` фрагментами (docs/architecture.md, «Узлы —
    // фрагменты в `domain`»), а здесь остаётся то, что знает про железо:
    // `boot:`-объект, блок `watchdog:` и список фрагментов. Растёт проект —
    // растёт список, а не тело графа.
    //
    // Порядок items фиксированный: `boot:` → `watchdog:` → `fragments:`
    // последним. Имя узлу даёт запись списка (`as HEARTBEAT`), не фрагмент: тот
    // же фрагмент под двумя `as` — два узла. Всё остальное, что объявляет
    // фрагмент (слоты, `provide_*`), получает префикс этого имени.
{%- if ota == "true" %}
    // `boot:`-объект — плата и свойства образа (`Boot` ниже): их отдаёт задача
    // `start` бинарника, у рабочего и тестового образа они свои.
    boot: boot: Boot;
{%- else %}
    boot: board: bsp::Board;
{%- endif %}

    watchdog: bsp::wdg::BoardWatchdog = {{ board }}.watchdog.arm(),
        check_every: WATCHDOG_CHECK_EVERY, observe: report_liveness;

    fragments: [
        ::domain::HEARTBEAT_FRAG as HEARTBEAT,

        // Узел `CPU_LOAD` меряет загрузку процессора. Вход — счётчик тиков сна,
        // который ведёт ручной цикл исполнителя в `main` бинарника (docs/modules/
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
        // (`<Updater, Reboot, …>`) — типы его слотов: слот — `static`, тип ему
        // нужен, а знает его только эта сторона; третий, число, — места ожидания
        // входа `CMD` (`OTA_CMD_PARKED` ниже). Привязка (`= …`) — вход узла
        // (`domain::ota::OtaWiring`): адаптер обновления над разделами `{{ board }}.firmware`, сброс МК
        // `bsp::reset::Reboot` и свойства этого образа из `boot.image` (защита от
        // отката: образ не новее неё узел отвергает). Образ узел принимает командами на свой вход
        // `CMD`{% if has_modbus != "true" and has_protobuf != "true" and has_canopen != "true" %}: клиента (`::domain::ota::OtaClient` над `ota_cmd_requester()`) получает ваш
        // протокол — без него узел только подтверждает образ{% else %} — их шлют узлы протоколов ниже, каждый со своим номером
        // транспорта (сессию приёма держит один клиент, остальным — `Busy`){% endif %}{% if has_canopen == "true" %}; узлы
        // CANopen — на вход `ASK`, не дожидаясь исхода (ребро `asks:` в их фрагменте){% endif %}; алгоритм обновления — в
        // `domain::ota`. Вычисляется привязка в прологе `spawn_all` первой
        // строкой — раньше инициализатора блока `watchdog:`, как бы они ни стояли
        // в тексте, — частичным move: `firmware` уезжает в слот узла, `watchdog`
        // следом забирает сторож, остаток `boot` дропается в конце пролога.
        ::domain::OTA_FRAG<::domain::ota::Updater<bsp::FirmwareFlash>, bsp::reset::Reboot, OTA_CMD_PARKED> as OTA = ::domain::ota::OtaWiring {
            flash: ::domain::ota::Updater::new({{ board }}.firmware),
            reboot: bsp::reset::Reboot,
            running_version: boot.image.version,
            // Тестовый образ `app-unconfirmed` начинает «уже подтверждённым» и
            // `mark_booted` не зовёт: следующий сброс его откатывает
            // ({% if has_protobuf == "true" %}`cargo xtask test host-target`, {% endif %}docs/ota.md).
            confirmed: boot.image.confirmed,
        },
{%- endif %}
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}

        // Узел `USB_{{ f[4] | upcase }}` — фоновая работа устройства `embassy-usb` (`domain::usb`),
        // которое собрала плата (`{{ board }}.{{ f[4] }}.device`, `bsp::usb::{{ f[4] }}`): без него хост
        // не видит устройства, а поток CDC — ни одного пакета.
        ::domain::USB_FRAG<bsp::usb::{{ f[4] }}::Device> as USB_{{ f[4] | upcase }} =
            ::domain::usb::UsbWiring { device: {{ board }}.{{ f[4] }}.device },
{%- endif %}
{%- endfor %}
{%- if has_net == "true" %}

        // Узел `NET` — фоновая работа стека `embassy-net` (`domain::net`) над
        // устройством платы `bsp::net::Device`: без него сокеты протоколов не
        // получают ни одного пакета. Один на `tcp` и `udp`: стек у них общий.
        ::domain::NET_FRAG<::domain::net::Runner<'static, bsp::net::Device>> as NET =
            ::domain::net::NetWiring { runner: {{ board }}.net.runner },
{%- endif %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, тот же, что у задачи без графа (`crate::tasks`). {% endcomment %}
{%- assign client = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- comment %} Канал — конкретным типом прямо в привязке: тип (`ty`) и конструктор (`link`) над рычагом платы в памяти, которую объявляет граф (`static T`). {% endcomment %}
{%- assign port = "502" %}
{%- if f[3] == "protobuf" %}{% assign port = "5000" %}{% endif %}
{%- if f[0] == "uart" %}
{%- capture src %}{{ board }}.{{ f[4] }}{% endcapture %}
{%- capture srct %}bsp::serial::{{ f[4] }}::Serial{% endcapture %}
{%- elsif f[0] == "usb" %}
{%- capture src %}{{ board }}.{{ f[4] }}.cdc{% endcapture %}
{%- capture srct %}bsp::usb::{{ f[4] }}::Cdc{% endcapture %}
{%- endif %}
{%- if f[3] == "modbus" %}
{%- if f[0] == "tcp" %}
{%- capture ty %}::adapters_logic::modbus::Tcp<::adapters_logic::net::TcpServer>{% endcapture %}
{%- capture link %}::adapters_logic::modbus::Tcp::new(::adapters_logic::net::TcpServer::new(
                    {{ board }}.net.stack,
                    {{ port }},
                    static [u8; SOCKET_BUFFER],
                    static [u8; SOCKET_BUFFER],
                )){% endcapture %}
{%- elsif f[0] == "udp" %}
{%- capture ty %}::adapters_logic::modbus::Udp<::adapters_logic::net::UdpLink>{% endcapture %}
{%- capture link %}::adapters_logic::modbus::Udp::new(::adapters_logic::net::UdpLink::new(
                    {{ board }}.net.stack,
                    {{ port }},
                    static ::core::mem::MaybeUninit<[::embassy_net::udp::PacketMetadata; 4]>,
                    static [u8; SOCKET_BUFFER],
                    static ::core::mem::MaybeUninit<[::embassy_net::udp::PacketMetadata; 4]>,
                    static [u8; SOCKET_BUFFER],
                    static [u8; ::adapters_logic::modbus::DATAGRAM],
                )){% endcapture %}
{%- else %}
{%- capture ty %}::adapters_logic::modbus::Rtu<{{ srct }}>{% endcapture %}
{%- capture link %}::adapters_logic::modbus::Rtu::new({{ src }}){% endcapture %}
{%- endif %}
{%- elsif f[3] == "protobuf" %}
{%- if f[0] == "udp" %}
{%- capture ty %}::adapters_logic::net::UdpLink{% endcapture %}
{%- capture link %}::adapters_logic::net::UdpLink::new(
                    {{ board }}.net.stack,
                    {{ port }},
                    static ::core::mem::MaybeUninit<[::embassy_net::udp::PacketMetadata; 4]>,
                    static [u8; SOCKET_BUFFER],
                    static ::core::mem::MaybeUninit<[::embassy_net::udp::PacketMetadata; 4]>,
                    static [u8; SOCKET_BUFFER],
                    static [u8; ::domain::protobuf::REQUEST_MAX],
                ){% endcapture %}
{%- else %}
{%- if f[0] == "tcp" %}
{%- capture src %}::adapters_logic::net::TcpServer::new(
                        {{ board }}.net.stack,
                        {{ port }},
                        static [u8; SOCKET_BUFFER],
                        static [u8; SOCKET_BUFFER],
                    ){% endcapture %}
{%- capture srct %}::adapters_logic::net::TcpServer{% endcapture %}
{%- endif %}
{%- capture ty %}::adapters_logic::cobs::Cobs<'static, {{ srct }}>{% endcapture %}
{%- capture link %}::adapters_logic::cobs::Cobs::new(
                    {{ src }},
                    static [u8; ::adapters_logic::cobs::rx_len(::domain::protobuf::REQUEST_MAX)],
                    static [u8; ::adapters_logic::cobs::tx_len(::domain::protobuf::RESPONSE_MAX)],
                ){% endcapture %}
{%- endif %}
{%- endif %}
{%- comment %} Что значат числа канала — одной строкой над узлом. {% endcomment %}
{%- capture sizes %}
{%- if f[0] == "tcp" or f[0] == "udp" %}
        // Порт {{ port }} — {% if f[3] == "modbus" %}стандартный порт Modbus TCP и UDP{% else %}выбран шаблоном, у protobuf стандартного нет{% endif %}; буферы сокета —
        // `SOCKET_BUFFER` (ниже){% if f[0] == "udp" %};
        // очередь — 4 датаграммы в каждую сторону, буфер канала — {% if f[3] == "modbus" %}самая длинная датаграмма{% else %}запрос{% endif %}{% endif %}.
{%- endif %}
{%- if f[3] == "protobuf" and f[0] != "udp" %}
        // Буфер канала — закодированные сообщение и CRC (`rx_len`), кадр передачи —
        // худший случай COBS от самого длинного ответа (`tx_len`).
{%- endif %}
{%- endcapture %}
{%- if f[3] == "modbus" %}

        // Узел `MODBUS_{{ f[4] | upcase }}` — Modbus slave (`domain::server` с обработчиком
        // `domain::modbus::Slave`): канал (кадрирование{% if f[0] == "tcp" or f[0] == "udp" %}, сокет{% endif %}) строится здесь{% if f[0] == "uart" or f[0] == "usb" %} над рычагом
        // платы{% endif %} в памяти, которую объявляет граф (`static T`); карта регистров
        // одна на все узлы Modbus: ручку `crate::modbus::handle()` (`Copy`) получает каждый, а
        // тип карты проект меняет в одном месте — `crate::modbus::Map`.
        // Адрес slave'а — `unit`{% if f[0] == "tcp" or f[0] == "udp" %}: по сети устройство адресуется по IP, и
        // slave отвечает на любой адрес в MBAP{% endif %}.{{ sizes }}
{%- if ota == "true" %}
        // Карта обёрнута окном OTA (`domain::modbus::OtaRegisters`): записи FC16
        // по `OTA_BASE` уходят узлу `OTA` командами — клиентом его входа `CMD`
        // (`OtaClient` с номером этого транспорта), остальное — карте проекта.
        ::domain::SERVER_FRAG<
            {{ ty }},
            ::domain::modbus::OtaRegisters<crate::modbus::Handle>,
        > as MODBUS_{{ f[4] | upcase }} =
            ::domain::server::ServerWiring {
                link: {{ link }},
                handler: ::domain::modbus::Slave::new(
                    1,
                    ::domain::modbus::OtaRegisters::new(
                        crate::modbus::handle(),
                        ::domain::ota::OtaClient::new(
                            ota_cmd_requester(),
                            ::domain::ota::ClientId({{ client }}),
                        ),
                    ),
                    boot.image.text,
                ),
            },
{%- else %}
        ::domain::SERVER_FRAG<{{ ty }}, crate::modbus::Handle> as MODBUS_{{ f[4] | upcase }} =
            ::domain::server::ServerWiring {
                link: {{ link }},
                handler: ::domain::modbus::Slave::new(1, crate::modbus::handle(), crate::build::PKG_VERSION),
            },
{%- endif %}
{%- elsif f[3] == "canopen" %}

        // Узел `CANOPEN_{{ f[4] | upcase }}` — узел CANopen (стек `zencan`) на шине CAN платы `{{ board }}.{{ f[4] }}`
        // (заглушка, пока проект не подставит свой контроллер — docs/modules/bsp-can.md)
        // и словаре объектов из `crates-host/canopen` (`device.toml`). Адрес узла в
        // сети — `node_id`, 1..=127. Хранилище — {% if config == "true" %}раздел настроек платы: в нём узел сохраняет
        // объекты по команде `save` и адрес, назначенный по LSS (ключи
        // `domain::canopen::{OBJECTS_KEY, NODE_ID_KEY}`){% else %}раздела настроек нет — объекты и адрес не сохраняются{% endif %}.
        ::domain::CANOPEN_FRAG<bsp::can::{{ f[4] }}::Can, ::domain::settings::Settings<{% if config == "true" %}bsp::ConfigFlash{% else %}::domain::settings::NoFlash{% endif %}>> as CANOPEN_{{ f[4] | upcase }} =
            ::domain::canopen::CanOpenWiring {
                bus: {{ board }}.{{ f[4] }},
                node_id: 1,
                dictionary: ::domain::canopen::dictionary(),
                settings: {% if config == "true" %}Some(::domain::settings::Settings::whole({{ board }}.config)){% else %}None{% endif %},
                // Серийный номер идентичности (0x1018:04) — из заводского номера чипа.
                serial: ::domain::canopen::serial({{ board }}.spec.uid),
{%- if ota == "true" %}
                // Команды объекта OTA словаря (0x4F00) узел шлёт на вход `ASK`
                // узла `OTA` (ребро `asks:` фрагмента) с номером этого транспорта.
                ota_client: ::domain::ota::ClientId({{ client }}),
{%- endif %}
            },
{%- elsif f[3] == "protobuf" %}

        // Узел `PROTOBUF_{{ f[4] | upcase }}` — сообщения protobuf от хоста (`domain::protobuf`): канал
        // строится здесь{% if f[0] == "uart" or f[0] == "usb" %} над рычагом платы{% endif %}, память канала и буфер ответа объявляет граф (`static T`);
        // обработчик `domain::protobuf::Service` над схемой `crates-host/proto` —
        // логика приложения, проект правит его в `domain`.{% if ota == "true" %} Команды обновления
        // обработчик шлёт узлу `OTA` на вход `CMD` своим клиентом (номер этого
        // транспорта) и отвечает хосту их исходом.{% endif %}{{ sizes }}
        ::domain::PROTOBUF_FRAG<{{ ty }}> as PROTOBUF_{{ f[4] | upcase }} =
            ::domain::protobuf::ProtobufWiring {
                link: {{ link }},
                // Ответ кодируется сюда, а не в буфер канала: там может лежать
                // недочитанный кадр, и ответ уходит через `send_from`.
                response: static [u8; ::domain::protobuf::RESPONSE_MAX],
                service: ::domain::protobuf::Service::new(
                    {{ info }},
{%- if ota == "true" %}
                    ::domain::ota::OtaClient::new(ota_cmd_requester(), ::domain::ota::ClientId({{ client }})),
{%- endif %}
                ),
            },
{%- endif %}
{%- if f[3] != "none" %}{% assign client = client | plus: 1 %}{% endif %}
{%- endfor %}
{%- if net_services contains "rest" %}

        // Узлы `HTTP` — REST API (`domain::http`): пул, по копии на соединение
        // (`crate::net::rest::CONNECTIONS`), у каждой свой сокет на стеке
        // `{{ board }}.net.stack` и свои буферы (слоты `= static` фрагмента).
        // Токен — `crate::net::rest::TOKEN` (файл `rest-token.txt`).{% if ota == "true" %}
        // Перезагрузку узлы просят у узла `OTA` каждый своим клиентом — номера
        // следом за транспортами: сессия приёма у OTA — одного клиента.{% endif %}
        ::domain::HTTP_FRAG as HTTP[{{ http_connections }}] = core::array::from_fn(|{% if ota == "true" %}i{% else %}_{% endif %}| ::domain::http::HttpWiring {
            stack: {{ board }}.net.stack,
            info: {{ info }},
            token: crate::net::rest::TOKEN,
{%- if ota == "true" %}
            ota: ::domain::ota::OtaClient::new(
                ota_cmd_requester(),
                ::domain::ota::ClientId(
                    {% if rest_ota_client != "0" %}{{ rest_ota_client }} + {% endif %}u8::try_from(i).expect("копий пула HTTP меньше 256"),
                ),
            ),
{%- endif %}
        }),
{%- endif %}
{%- if net_services contains "ssh" %}

        // Узел `SSH` — консоль (`domain::ssh`): своё соединение на стеке
        // `{{ board }}.net.stack`, буферы — слоты `= static` фрагмента. Вход —
        // `crate::net::ssh::LOGIN`; ключ хоста — {% if ssh_host_key == "config" %}создаётся платой при первом старте и
        // хранится в разделе настроек (`domain::ssh::Stored`){% else %}вшит из `ssh-host-key.bin`
        // (`crate::net::ssh::HOST_KEY`){% endif %}.{% if ota == "true" %} `reboot` — командой узлу `OTA` своим
        // клиентом, номер следом за остальными.{% endif %}
        ::domain::SSH_FRAG{% if ssh_host_key == "config" %}<::domain::ssh::Stored<bsp::ConfigFlash>>{% endif %} as SSH =
            ::domain::ssh::SshWiring {
                stack: {{ board }}.net.stack,
                info: {{ info }},
                login: crate::net::ssh::LOGIN,
{%- if ssh_host_key == "config" %}
                host_key: ::domain::ssh::Stored(::domain::settings::Settings::whole({{ board }}.config)),
{%- else %}
                host_key: &crate::net::ssh::HOST_KEY,
{%- endif %}
{%- if ota == "true" %}
                ota: ::domain::ota::OtaClient::new(
                    ota_cmd_requester(),
                    ::domain::ota::ClientId({{ ssh_ota_client }}),
                ),
{%- endif %}
            },
{%- endif %}
{%- if net_services contains "mqtt" %}

        // Узел `MQTT` — клиент брокера (`domain::mqtt`): адрес и префикс тем
        // — из `mqtt.toml` (`crate::net::mqtt::BROKER`, `PREFIX`),
        // соединение — TCP-клиент на стеке `{{ board }}.net.stack` в буферах, которые
        // объявляет граф (`static T`), буферы `minimq` — слоты
        // `= static` фрагмента.{% if ota == "true" %} `cmd/reboot` — командой узлу `OTA` своим
        // клиентом, номер следом за остальными.{% endif %}
        ::domain::MQTT_FRAG as MQTT =
            ::domain::mqtt::MqttWiring {
                stack: {{ board }}.net.stack,
                info: {{ info }},
                prefix: crate::net::mqtt::PREFIX,
                broker: crate::net::mqtt::BROKER,
                link: ::domain::mqtt::TcpClient::new(
                    {{ board }}.net.stack,
                    static [u8; crate::net::mqtt::TCP_BUFFER],
                    static [u8; crate::net::mqtt::TCP_BUFFER],
                ),
{%- if ota == "true" %}
                ota: ::domain::ota::OtaClient::new(
                    ota_cmd_requester(),
                    ::domain::ota::ClientId({{ mqtt_ota_client }}),
                ),
{%- endif %}
            },
{%- endif %}
    ];
}
{%- if net_services contains "rest" %}

// Пул `HTTP[N]` в `fragments:` — литерал: DSL не берёт константу. Генерация
// пишет его тем же числом, что `HTTP_CONNECTIONS` в `[env]`; ручная правка
// одного без другого — ошибка здесь, а не нехватка сокетов на плате.
const _: () = assert!(
    crate::net::rest::CONNECTIONS == {{ http_connections }},
    "копий HTTP в графе столько же, сколько HTTP_CONNECTIONS в [env]"
);
{%- endif %}
{%- if protocol_sockets != "0" %}

/// Буфер сокета протокола в каждую сторону, байт — с запасом на {% if modbus_mbap == "true" %}самый
/// длинный кадр Modbus TCP (260 байт){% endif %}{% if modbus_mbap == "true" and has_protobuf == "true" %} и {% endif %}{% if has_protobuf == "true" %}сообщение protobuf (`domain::protobuf::REQUEST_MAX`,
/// `RESPONSE_MAX`){% endif %}. Одно число на все сокеты протоколов в `fragments:` выше.
const SOCKET_BUFFER: usize = 1024;
{%- endif %}
{%- if ota == "true" %}

// Мест ожидания входа `CMD` узла `OTA` (`RpcService`) — по его клиентам, а не
// умолчание `sync-request` (4): иначе ждущие сверх четырёх будят друг друга
// по кругу. Клиентов — {{ ota_cmd_clients }}: узлы протоколов, кроме CANopen
// (тот на `ASK`), копии `HTTP`, `SSH`, `MQTT`; число считает генерация.
// Подключили к `CMD` своего клиента — прибавьте его к числу здесь. Константа, а не
// выражение в `fragments:`: аргумент фрагмента — литерал или одно имя.
const OTA_CMD_PARKED: usize = ::domain::ota::cmd_parked({{ ota_cmd_clients }});
{%- endif %}

/// Как часто тикер графа опрашивает сторожей узлов и кормит железо.
///
/// Единственная цифра графа, которая живёт здесь: она про один аппаратный
/// сторож на всех, а не про какой-то узел. Цепочка таймаутов и где какое её
/// звено проверяется — `docs/watchdog.md`; правое звено — `const`-assert ниже.
const WATCHDOG_CHECK_EVERY: Duration = Duration::from_millis(100);

// Правое звено цепочки таймаутов, и единственное место в шаблоне, где видны оба
// его конца: таймаут узла живёт в `domain`, аппаратный — в `bsp`. `supervisor`
// намеренно не связывает дедлайн узла с таймаутом железа (граф с длинным
// дедлайном корректен сам по себе), а шаблон — должен: иначе `check_every`
// укладывается, все гейты зелёные, а IWDG сбрасывает МК раньше, чем граф
// заметит застой узла, и сброс приходит от железа — ни один узел не назван причиной, а
// диагностика `supervisor` оказывается мёртвым кодом.
//
// Узлов с `watchdog:` в списке `fragments:` выше ровно два; новый фрагмент со
// своим `watchdog:` обязан встать в этот assert — иначе его дедлайн проверит
// один только этот комментарий. `<` на `Duration` в `const` не работает (нет
// const-трейтов), поэтому сравниваются микросекунды — как в `HW_TIMEOUT_US`,
// а не тики, чья частота зависит от фичи `tick-hz-*`.
//
// Справа — не номинал `HW_TIMEOUT_US`, а `HW_TIMEOUT_MIN_US`: таймаут при LSI
// на верхнем пределе семейства. Embassy заряжает IWDG от номинальной частоты
// LSI, а реальная плавает (у F4 до 47 кГц при номинале 32), и на номинале
// цепочка сходилась бы там, где на быстром LSI железо её опережает.
const _: () = assert!(
    ::domain::heartbeat::WATCHDOG.as_micros() + WATCHDOG_CHECK_EVERY.as_micros()
        < bsp::wdg::HW_TIMEOUT_MIN_US
        && ::domain::cpu_load::WATCHDOG.as_micros() + WATCHDOG_CHECK_EVERY.as_micros()
            < bsp::wdg::HW_TIMEOUT_MIN_US,
    "таймаут узла плюс WATCHDOG_CHECK_EVERY обязан быть меньше bsp::wdg::HW_TIMEOUT_MIN_US (таймаут \
     IWDG при самом быстром LSI): иначе железо сбросит МК раньше, чем узел заметит зависшую задачу, и \
     сброс придёт от железа, а не от supervisor"
);

{%- if ota == "true" %}

/// `boot:`-объект графа: что `spawn_all` получает от задачи `start` бинарника.
///
/// Плата — рычаги для узлов, свойства образа — то, чем рабочий `app` и
/// тестовый `app-unconfirmed` различаются (`crate::version`). Инициализаторы
/// `fragments:` и `watchdog:` забирают из него поля частичными move.
pub struct Boot {
    /// Плата целиком: её поля делят узлы.
    pub board: bsp::Board,
    /// Свойства образа: версия и подтверждение — узлу OTA, версия текстом —
    /// в сведения об устройстве{% if has_modbus == "true" %} и Modbus{% endif %}.
    pub image: crate::version::Image,
}
{%- endif %}

/// Куда уходит просрочка наблюдаемого узла.
///
/// Наблюдатель — часть декларации графа (`observe:` в блоке `watchdog:`), а не
/// вызов (docs/watchdog.md).
///
/// Печатью список действий не исчерпывается, он с неё начинается: что делать
/// с зависшим узлом — деградировать, поднять тревогу, сбросить МК —
/// это доменное знание, которого у сторожа нет.
fn report_liveness(node: &'static str, liveness: Liveness) {
    match liveness {
        Liveness::Stale => warn!("app: узел {} перестал отмечаться", node),
        Liveness::Recovered => info!("app: узел {} отметился снова", node),
    }
}

{%- endif %}
