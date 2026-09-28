#![doc = include_str!("../../../docs/modules/app-graph.md")]

{%- if graph == "true" %}
use defmt::{info, warn};
use supervisor::{Duration, Liveness, supervisor_graph};

/// Как часто тикер графа опрашивает сторожей узлов и кормит железо.
///
/// Единственная цифра графа, которая живёт здесь: она про один аппаратный
/// сторож на всех, а не про какой-то узел. Таймаут узла — во фрагменте
/// (`domain::app::WATCHDOG`), аппаратный — у железа (`bsp::wdg::HW_TIMEOUT_US`),
/// и соотношение между ними держите сами: `WATCHDOG + WATCHDOG_CHECK_EVERY <
/// HW_TIMEOUT_US`. Компилятор проверяет только `check_every` против
/// аппаратного таймаута — `supervisor_graph!` эмитит `const`-assert, читая
/// `HARDWARE_TIMEOUT` у типа сторожа; левое звено цепочки (потолок backoff'а
/// < таймаут узла) держит `const`-assert в `domain::app` — `docs/watchdog.md`.
const WATCHDOG_CHECK_EVERY: Duration = Duration::from_millis(100);

supervisor_graph! {
    // Узлы приезжают из `domain`: каждая подсистема объявляет свой фрагмент
    // (`supervisor_fragment!`) рядом со своей задачей — вместе с политикой
    // перезапуска и таймаутом, — а здесь остаётся то, что знает про железо:
    // `boot:`-объект, блок `watchdog:` и список фрагментов. Растёт проект —
    // растёт список, а не тело графа.
    //
    // Порядок items фиксированный: `boot:` → `watchdog:` → `fragments:`
    // последним. Имя узлу даёт запись списка (`as APP`), не фрагмент: тот же
    // фрагмент под двумя `as` — два узла. Всё остальное, что объявляет
    // фрагмент (слоты, `provide_*`), получает префикс этого имени.
    boot: board: bsp::Board;

    watchdog: bsp::wdg::BoardWatchdog = board.watchdog.arm(),
        check_every: WATCHDOG_CHECK_EVERY, observe: report_liveness;
{%- if ota == "true" %}

    // Фрагмент OTA — с параметрами и своей `boot:`-привязкой. Параметры
    // (`<Link, Updater>`) — типы двух его слотов: слот — `static`, тип ему
    // нужен, а знает его только эта сторона; подставили свой транспорт вместо
    // заглушки — меняйте первый аргумент. Привязка (`= …`) — поле `board.ota`,
    // уже собранное платой в нужную фрагменту форму{% if signed == "true" %} и обёрнутое здесь
    // версией и ключом прошивки (`crate::ota::signed`){% endif %}. Вычисляется она в
    // прологе `spawn_all` первой строкой — раньше инициализатора блока
    // `watchdog:`, как бы они ни стояли в тексте, — частичным move: `ota`
    // уезжает в слоты узла, `watchdog` следом забирает сторож, остаток
    // `board` дропается в конце пролога.
{%- endif %}
    fragments: [::domain::APP_FRAG as APP{% if ota == "true" %}, {% if signed == "true" %}::domain::OTA_SIGNED_FRAG<bsp::ota::Link, crate::ota::Signed> as OTA = crate::ota::signed(board.ota){% else %}::domain::OTA_FRAG<bsp::ota::Link, bsp::ota::Updater> as OTA = board.ota{% endif %}{% endif %}];
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
