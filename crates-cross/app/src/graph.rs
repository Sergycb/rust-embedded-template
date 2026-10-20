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
use supervisor::{Duration, Liveness, supervisor_graph};

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
        // (`<Updater, Reboot>`) — типы его слотов: слот — `static`, тип ему
        // нужен, а знает его только эта сторона. Привязка (`= …`) — вход узла
        // (`domain::ota::OtaWiring`): адаптер обновления `board.ota`, сброс МК
        // `bsp::cpu::Reboot` и режим применения (`Mode::{% if signed == "true" %}Signed` с версией этого образа
        // из `crate::image`{% else %}Plain`{% endif %}). Образ узел принимает командами на свой вход
        // `CMD`{% if has_modbus != "true" and has_protobuf != "true" and has_canopen != "true" %}: клиента отдаёт `ota_cmd_requester()`, и шлёт их ваш
        // протокол — без него узел только подтверждает образ{% else %} — их шлют узлы протоколов ниже, каждый своим клиентом{% endif %}; алгоритм обновления — в
        // `domain::ota`. Вычисляется привязка в прологе `spawn_all` первой
        // строкой — раньше инициализатора блока `watchdog:`, как бы они ни стояли
        // в тексте, — частичным move: `ota` уезжает в слот узла, `watchdog`
        // следом забирает сторож, остаток `board` дропается в конце пролога.
        ::domain::OTA_FRAG<bsp::ota::Updater, bsp::cpu::Reboot> as OTA = ::domain::ota::OtaWiring {
            flash: board.ota,
            reboot: bsp::cpu::Reboot,
{%- if signed == "true" %}
            mode: ::domain::ota::Mode::Signed {
                running_version: crate::image::FW_VERSION,
            },
{%- else %}
            mode: ::domain::ota::Mode::Plain,
{%- endif %}
        },
{%- endif %}
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}

        // Узел `USB_{{ f[4] | upcase }}` — фоновая работа устройства `embassy-usb` (`domain::usb`)
        // над драйвером `bsp::usb::{{ f[4] }}::UsbDriver`: без него хост не видит
        // устройства, а поток CDC — ни одного пакета.
        ::domain::USB_FRAG<bsp::usb::{{ f[4] }}::Device> as USB_{{ f[4] | upcase }} =
            ::domain::usb::UsbWiring { device: board.{{ f[4] }}.device },
{%- endif %}
{%- endfor %}
{%- if has_net == "true" %}

        // Узел `NET` — фоновая работа стека `embassy-net` (`domain::net`) над
        // устройством платы `bsp::net::Device`: без него сокеты протоколов не
        // получают ни одного пакета. Один на `tcp` и `udp`: стек у них общий.
        ::domain::NET_FRAG<::domain::net::Runner<'static, bsp::net::Device>> as NET =
            ::domain::net::NetWiring { runner: board.net.runner },
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "modbus" %}

        // Узел `MODBUS_{{ f[4] | upcase }}` — Modbus slave: транспорт собирает `crate::link::{{ f[4] }}`
        // (кадрирование{% if f[0] == "tcp" or f[0] == "udp" %}, сокет{% endif %}) над тем, что отдала плата; карта регистров
        // `domain::modbus::EmptyRegisters` пуста, её проект описывает в `domain`.
        // Адрес slave'а — `unit`{% if f[0] == "tcp" or f[0] == "udp" %}: по сети устройство адресуется по IP, и
        // slave отвечает на любой адрес в MBAP{% endif %}.
{%- if ota == "true" %}
        // Карта обёрнута окном OTA (`domain::modbus::OtaRegisters`): записи FC16
        // по `OTA_BASE` уходят узлу `OTA` командами — клиентом его входа `CMD`
        // (`ota_cmd_requester()`), остальное — карте проекта.
        ::domain::MODBUS_SLAVE_FRAG<
            crate::link::{{ f[4] }}::Link,
            ::domain::modbus::OtaRegisters<::domain::modbus::EmptyRegisters>,
        > as MODBUS_{{ f[4] | upcase }} =
            ::domain::modbus::ModbusSlaveWiring {
                link: crate::link::{{ f[4] }}::new({% if f[0] == "uart" %}board.{{ f[4] }}{% elsif f[0] == "usb" %}board.{{ f[4] }}.cdc{% else %}board.net.stack{% endif %}),
                regs: ::domain::modbus::OtaRegisters::new(
                    ::domain::modbus::EmptyRegisters,
                    ota_cmd_requester(),
                ),
                unit: 1,
            },
{%- else %}
        ::domain::MODBUS_SLAVE_FRAG<crate::link::{{ f[4] }}::Link, ::domain::modbus::EmptyRegisters> as MODBUS_{{ f[4] | upcase }} =
            ::domain::modbus::ModbusSlaveWiring {
                link: crate::link::{{ f[4] }}::new({% if f[0] == "uart" %}board.{{ f[4] }}{% elsif f[0] == "usb" %}board.{{ f[4] }}.cdc{% else %}board.net.stack{% endif %}),
                regs: ::domain::modbus::EmptyRegisters,
                unit: 1,
            },
{%- endif %}
{%- elsif f[3] == "canopen" %}

        // Узел `CANOPEN_{{ f[4] | upcase }}` — узел CANopen (стек `zencan`) на шине CAN платы `board.{{ f[4] }}`
        // (заглушка, пока проект не подставит свой контроллер — docs/modules/bsp-can.md)
        // и словаре объектов из `crates-host/canopen` (`device.toml`). Адрес узла в
        // сети — `node_id`, 1..=127. Хранилище — {% if config == "true" %}раздел настроек платы: в нём узел сохраняет
        // объекты по команде `save` и адрес, назначенный по LSS (ключи
        // `domain::canopen::{OBJECTS_KEY, NODE_ID_KEY}`){% else %}раздела настроек нет — объекты и адрес не сохраняются{% endif %}.
        ::domain::CANOPEN_FRAG<bsp::can::{{ f[4] }}::Can, {% if config == "true" %}bsp::settings::Settings{% else %}::adapters::settings::Unavailable{% endif %}> as CANOPEN_{{ f[4] | upcase }} =
            ::domain::canopen::CanOpenWiring {
                bus: board.{{ f[4] }},
                node_id: 1,
                dictionary: ::domain::canopen::dictionary(),
                settings: {% if config == "true" %}board.settings{% else %}::adapters::settings::Unavailable{% endif %},
                persist: {{config}},
{%- if ota == "true" %}
                // Команды объекта OTA словаря (0x4F00) узел отдаёт узлу `OTA`
                // клиентом входа `CMD`.
                ota_cmd: ota_cmd_requester(),
{%- endif %}
            },
{%- elsif f[3] == "protobuf" %}

        // Узел `HOST_{{ f[4] | upcase }}` — сообщения protobuf от хоста: канал и буферы сообщений
        // заводит `crate::link::{{ f[4] }}` над тем, что отдала плата;
        // обработчик `domain::host::Service` над схемой `crates-host/proto` —
        // логика приложения, проект правит его в `domain`.{% if ota == "true" %} Команды обновления
        // он отдаёт узлу `OTA` клиентом его входа `CMD` (`ota_cmd_requester()`).{% endif %}
        ::domain::HOST_FRAG<crate::link::{{ f[4] }}::Link, ::domain::host::Service> as HOST_{{ f[4] | upcase }} =
            ::domain::host::HostWiring {
                link: crate::link::{{ f[4] }}::new({% if f[0] == "uart" %}board.{{ f[4] }}{% elsif f[0] == "usb" %}board.{{ f[4] }}.cdc{% else %}board.net.stack{% endif %}),
                service: ::domain::host::Service::new(
                    crate::device_info(board.spec),
{%- if ota == "true" %}
                    ota_cmd_requester(),
{%- endif %}
                ),
                rx: crate::link::{{ f[4] }}::rx(),
                tx: crate::link::{{ f[4] }}::tx(),
            },
{%- endif %}
{%- endfor %}
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
