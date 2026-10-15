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
{%- if protocol == "protobuf" %}
use static_cell::StaticCell;
{%- endif %}
use supervisor::{Duration, Liveness, supervisor_graph};
{%- if protocol == "protobuf" %}

/// Канал узла `HOST`: кадры COBS + CRC поверх UART платы.
type HostLink = adapters::cobs::Cobs<'static, bsp::serial::Serial>;

/// Самое длинное сообщение схемы, под которое заведены буферы узла `HOST`.
/// Схема с сообщениями длиннее — меняйте здесь; узел предупредит при старте,
/// если самый длинный ответ схемы не помещается.
const HOST_MESSAGE: usize = 256;
{%- endif %}

/// Как часто тикер графа опрашивает сторожей узлов и кормит железо.
///
/// Единственная цифра графа, которая живёт здесь: она про один аппаратный
/// сторож на всех, а не про какой-то узел. Таймаут узла — во фрагменте
/// (`domain::heartbeat::WATCHDOG`), аппаратный — у железа
/// (`bsp::wdg::HW_TIMEOUT_US`), и соотношение между ними проверяет
/// `const`-assert ниже. Компилятор проверяет сам `check_every` против
/// аппаратного таймаута — `supervisor_graph!` эмитит `const`-assert, читая
/// `HARDWARE_TIMEOUT` у типа сторожа; левое звено цепочки (потолок backoff'а <
/// таймаут узла) держит `const`-assert во фрагменте (`domain::heartbeat`) —
/// `docs/watchdog.md`.
const WATCHDOG_CHECK_EVERY: Duration = Duration::from_millis(100);

// Правое звено цепочки таймаутов, и единственное место в шаблоне, где видны оба
// его конца: таймаут узла живёт в `domain`, аппаратный — в `bsp`. `supervisor`
// намеренно не связывает дедлайн узла с таймаутом железа (граф с длинным
// дедлайном корректен сам по себе), а шаблон — должен: иначе `check_every`
// укладывается, все гейты зелёные, а IWDG сбрасывает МК в штатной паузе
// backoff'а, и сброс приходит от железа — ни один узел не назван причиной, а
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
{%- if ota == "true" %}

    // Фрагмент OTA — с параметрами и своей `boot:`-привязкой. Параметры
    // (`<Link, Updater>`) — типы двух его слотов: слот — `static`, тип ему
    // нужен, а знает его только эта сторона; подставили свой транспорт вместо
    // заглушки — меняйте первый аргумент. Привязка (`= …`) — вход узла
    // (`domain::ota::OtaWiring`), разложенный из поля платы `board.ota`, и режим
    // применения (`Mode::{% if signed == "true" %}Signed` с версией этого образа из
    // `crate::image`{% else %}Plain`{% endif %}): `bsp`
    // про `domain` не знает, поэтому раскладка здесь. Вычисляется она в
    // прологе `spawn_all` первой строкой — раньше инициализатора блока
    // `watchdog:`, как бы они ни стояли в тексте, — частичным move: `ota`
    // уезжает в слоты узла, `watchdog` следом забирает сторож, остаток
    // `board` дропается в конце пролога.
{%- endif %}
{%- if protocol == "modbus" %}

    // Узел `MODBUS` — Modbus slave поверх UART платы: транспорт RTU
    // (`adapters::modbus::Rtu`) вокруг `board.serial` (заглушка, пока проект
    // не подставит свой UART — docs/modules/bsp-serial.md) и карта регистров
    // `domain::modbus::Registers` — пустая, её проект описывает в `domain`.
    // Адрес slave'а на шине — `unit`.
{%- elsif protocol == "protobuf" %}

    // Узел `HOST` — сообщения protobuf от хоста поверх UART платы: кадры COBS +
    // CRC (`adapters::cobs::Cobs`) вокруг `board.serial` (заглушка, пока
    // проект не подставит свой UART — docs/modules/bsp-serial.md) и обработчик
    // `domain::host::Service` над схемой `crates-host/proto`. Буферы — на
    // `HOST_MESSAGE` байт: приёму нужно ещё место под CRC (`rx_len`), кадру
    // передачи — под худший случай COBS (`tx_len`).
{%- endif %}
    fragments: [::domain::HEARTBEAT_FRAG as HEARTBEAT,
    //
    // Узел `CPU_LOAD` меряет загрузку процессора. Вход — счётчик тиков сна,
    // который ведёт ручной цикл исполнителя в `main.rs` (docs/modules/
    // app-cpu-load.md); всё, что нужно фрагменту, — ссылка на него, поэтому
    // привязка такая же, как у узла OTA: тип входа объявляет подсистема
    // (`domain::cpu_load::IdleMeter`), а значение собирает приложение.
    //
    // Подписчика в шаблоне нет: слот `PERCENT` объявлен явно, единицей, и
    // первым его занимает тот, кто первым позовёт
    // `cpu_load_percent_receiver()`. Само значение в лог не идёт — читатель
    // должен быть, иначе процент некуда девать.
    ::domain::CPU_LOAD_FRAG as CPU_LOAD = ::domain::cpu_load::IdleMeter { sleep_ticks: crate::cpu_load::sleep_ticks() }{% if ota == "true" %}, ::domain::OTA_FRAG<bsp::ota::Link, bsp::ota::Updater> as OTA = ::domain::ota::OtaWiring { link: board.ota.link, flash: board.ota.flash, mode: {% if signed == "true" %}::domain::ota::Mode::Signed { running_version: crate::image::FW_VERSION }{% else %}::domain::ota::Mode::Plain{% endif %} }{% endif %}{% if protocol == "modbus" %}, ::domain::MODBUS_SLAVE_FRAG<::adapters::modbus::Rtu<bsp::serial::Serial>, ::domain::modbus::Registers> as MODBUS = ::domain::modbus::ModbusSlaveWiring { link: ::adapters::modbus::Rtu::new(board.serial), regs: ::domain::modbus::Registers, unit: 1 }{% elsif protocol == "protobuf" %}, ::domain::HOST_FRAG<HostLink, ::domain::host::Service> as HOST = ::domain::host::HostWiring { link: HostLink::new(board.serial, { static FRAME: StaticCell<[u8; HostLink::tx_len(HOST_MESSAGE)]> = StaticCell::new(); FRAME.init([0; HostLink::tx_len(HOST_MESSAGE)]) }), service: ::domain::host::Service::new(crate::build::PKG_VERSION), rx: { static RX: StaticCell<[u8; HostLink::rx_len(HOST_MESSAGE)]> = StaticCell::new(); RX.init([0; HostLink::rx_len(HOST_MESSAGE)]) }, tx: { static TX: StaticCell<[u8; HOST_MESSAGE]> = StaticCell::new(); TX.init([0; HOST_MESSAGE]) } }{% endif %}];
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
