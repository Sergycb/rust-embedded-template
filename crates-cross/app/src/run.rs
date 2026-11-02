//! Тело прошивки, общее у бинарников пакета: [`run`] — запуск и ручной цикл
//! исполнителя с замером сна (docs/modules/app-cpu-load.md), задача `start` —
//! сборка платы и спавн узлов.

use defmt::{error, info};
// RTT остаётся дефолтным транспортом в обоих профилях — сырой шаблон не знает,
// какой конкретный USART на плате пользователь отведёт под лог без пробника.
// Паттерн замены (UART, без RTT/пробника) для release — в
// docs/modules/app.md, подключается по мере готовности платы (Board
// должен будет отдавать объект транспорта, собранный из реальной периферии).
use defmt_rtt as _;

use embassy_executor::Spawner;
use embassy_executor::raw::Executor;
use embassy_time::Instant;
use static_cell::StaticCell;

/// Исполнитель задач — свой, а не из `#[embassy_executor::main]` (почему — в
/// [`run`]). `StaticCell`, а не `ConstStaticCell`: `Executor::new` не `const`.
static EXECUTOR: StaticCell<Executor> = StaticCell::new();

{%- if has_protobuf == "true" or net_services != "" %}

/// Причина сброса этого запуска (`bsp::reset::take_cause`): пишет [`run`] до
/// `Board::new`, читает [`device_info`].
static RESET: cortex_m::interrupt::Mutex<core::cell::Cell<&'static str>> =
    cortex_m::interrupt::Mutex::new(core::cell::Cell::new("unknown"));

/// Сведения об устройстве для ответа хосту на `info`{% if net_services contains "rest" %} и REST `/api/info`{% endif %}{% if net_services contains "mqtt" %} и темы MQTT `info`{% endif %}: версия и
/// коммит — из `shadow-rs` (те же, что в баннере старта), прошлая паника —
/// из `panic::last`, причина сброса — из [`RESET`], остальное — с платы.
pub(crate) fn device_info(spec: bsp::ChipSpec) -> domain::device::DeviceInfo {
    domain::device::DeviceInfo {
        version: {% if ota == "true" %}crate::version::FW_VERSION_TEXT{% else %}crate::build::PKG_VERSION{% endif %},
        commit: crate::build::SHORT_COMMIT,
        chip: spec.chip,
        uid: spec.uid,
        flash_size: spec.flash_size,
        sys_hz: spec.sys_hz,
        reset: cortex_m::interrupt::free(|cs| RESET.borrow(cs).get()),
        last_panic: crate::panic::last()
            .map(|m| domain::device::panic_brief(m, domain::device::PANIC_BRIEF)),
    }
}
{%- endif %}

/// Тело прошивки: ловушки, баннер, причины сброса и падения, затем
/// исполнитель и его цикл. Зовёт его `#[entry]` бинарника — `main` в
/// `src/main.rs`{% if ota == "true" %} и `src/bin/app-unconfirmed.rs`{% endif %}.
///
/// # Panics
///
/// Задача `start` уже заспавнена — `run` позвали второй раз.
pub fn run() -> ! {
{%- if core_baseline != "true" %}
    // Первым делом: ловушка деления действует на всё, что выполнится дальше,
    // включая `Board::new` (см. `fault`).
    crate::fault::enable_div_0_trap();
{%- endif %}
    // Первая же строка лога отвечает на вопрос «а что вообще залито в плату»:
    // версия пакета и коммит, из которого собран образ (их подставляет
    // `shadow-rs` в build.rs). Без этого build-info собиралась бы впустую, а по
    // OTA легко получить плату с прошивкой, происхождение которой неизвестно.
    info!(
        "app: запуск {} ({})",
        {% if ota == "true" %}crate::version::FW_VERSION_TEXT{% else %}crate::build::PKG_VERSION{% endif %},
        crate::build::SHORT_COMMIT,
    );
{%- if ota == "true" %}
    // Тестовый образ `cargo xtask test host-target` (бинарник `app-unconfirmed`)
    // виден с первых строк лога: он себя не подтверждает, и следующий сброс
    // откатывает его (docs/ota.md, «Тесты OTA по протоколу»).
    if cfg!(feature = "unconfirmed") {
        defmt::warn!(
            "app: ТЕСТОВЫЙ образ app-unconfirmed: себя не подтверждает, следующий сброс его откатит"
        );
    }
{%- endif %}

    // Причина сброса — до `Board::new` и до всего, что может повиснуть или
    // упасть (docs/diagnostics.md, «Причина сброса»). Сброс из `ensure_*`
    // в `Board::new` ставит свой флаг заново — его прочитает следующий старт.
    let reset = bsp::reset::take_cause();
    info!("app: причина сброса {}", reset);
{%- if has_protobuf == "true" or net_services != "" %}
    cortex_m::interrupt::free(|cs| RESET.borrow(cs).set(reset));
{%- endif %}

    // Причина прошлого падения, если оно было: паникёр release-профиля успел
    // записать её в регион PANIC, а тот пережил сброс (RAM, но вне секций,
    // которые cortex-m-rt инициализирует при старте). Читается один раз —
    // `panic-persist` стирает свою магию, — поэтому печатать надо сразу и до
    // всего остального: следующий сброс эту строку уже не покажет.
    //
    // ДО `Board::new`, и это важно: он умеет паниковать сам
    // (негодная конфигурация тактирования, раздел `CONFIG` не на границах
    // страниц, нет клока у RNG), а в release паника это сброс. Печатай мы после
    // них — устройство, падающее внутри `new`, крутилось бы в цикле
    // перезагрузок молча.
    //
    // В dev паникёр — `panic-probe`, дамп никто не пишет, и `panic::take`
    // всегда `None`. `panic-persist` стирает свою магию при чтении, поэтому
    // `panic::take` запоминает прочитанное: сведения об устройстве берут ту
    // же строку позже (`panic::last`).
    if let Some(reason) = crate::panic::take() {
        error!("app: предыдущий запуск упал: {}", reason);
    }

    // Исполнитель — свой, а не `#[embassy_executor::main]`, и это единственная
    // причина, по которой `run` здесь синхронный: только так измеряется сон
    // (docs/modules/app-cpu-load.md, «Почему цикл исполнителя в `main` написан
    // руками»).
    //
    // `usize::MAX` вместо нуля — тот же «контекст», что и у макроса: на
    // thread-режиме `__pender` делает `SEV` (пока `executor-interrupt` не
    // включён, ровно так же), а не `WFE`, то есть пробуждение задач не теряет.
    // Цикл ниже ставит `WFE` сам, и всё, что остаётся на долю pender'а, — это
    // возврат из него, а не сон.
    let executor = EXECUTOR.init(Executor::new(usize::MAX as *mut ()));
    let spawner = executor.spawner();
    // Плату собирает и узлы спавнит первая задача, `start`, — до неё в цикле
    // ничего не выполняется.
    spawner.spawn(start(spawner).expect("задача start свежая: спавнится один раз"));

    // Цикл исполнителя: поллим, потом спим, пока нечего делать, и отмечаем в
    // счётчике, сколько тиков проспали. `wfe()` возвращается и по событию (SEV
    // от `spawn`, от пробуждения задачи), и по таймеру — от того, что сработал
    // следующий `Timer` задачи, — так что это единственное место, где цикл ждёт.
    // `poll` — первым: часы, которыми меряется сон, поднимает `Board::new` в
    // первом же проходе (`embassy_stm32::init`), и до него `Instant` читать
    // нечем.
    loop {
        // SAFETY: поллить исполнитель можно только из единственного места, а
        // оно здесь — этот цикл единственный, кто его крутит. Ровно то же
        // обещание, что у `Executor::run`.
        //
        // `#[expect(unsafe_code)]` — вместе с остальными в `crates-cross` (см.
        // `crates-cross/Cargo.toml`): unsafe в прошивке допустим, но каждое
        // место обязано быть перечислено и объяснено, а линт `unsafe_code`
        // заставляет перечислять.
        #[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
        unsafe {
            executor.poll();
        }
        let before = Instant::now().as_ticks();
        cortex_m::asm::wfe();
        crate::cpu_load::record_sleep_ticks(Instant::now().as_ticks().saturating_sub(before));
    }
}

/// Первая задача: собирает плату и спавнит узлы.
///
/// Задачей, а не в [`run`] до исполнителя: {% if external_flash == "true" %}`Board::new` ждёт таймерами
/// `embassy_time` (`init` внешней флеши){% else %}так `Board::new` может ждать таймерами
/// `embassy_time`, когда плате это понадобится (сейчас он синхронный){% endif %}, а встроенной очереди
/// таймеров нужен waker исполнителя — под `block_on` таймер паникует. Узлов в
/// этот момент ещё нет, так что `Board::new` по-прежнему выполняется раньше
/// всего остального.
{%- if external_flash != "true" %}
// `allow`, а не `expect`: `#[embassy_executor::task]` копирует атрибут и на
// внешнюю, синхронную функцию, где линт не срабатывает, и `expect` там остаётся
// неисполненным.
#[allow(
    clippy::unused_async,
    reason = "задача embassy обязана быть async, а без внешней флеши `Board::new` синхронный"
)]
{%- endif %}
#[embassy_executor::task]
async fn start(spawner: Spawner) {
    // Плата — рычаги для узлов.
    let board = bsp::Board::new(){% if external_flash == "true" %}.await{% endif %};
    // Следом — на чём именно: чип и его заводской номер, по которому
    // экземпляр платы отличается от соседнего.
    info!("app: {}", board.spec);
{%- if graph == "true" %}

    // Граф задач — единственное место, где в этом проекте появляются задачи.
    // Голый `spawner.spawn(..)` рядом с ним завёл бы вторую, неуправляемую
    // ветку: без `deps:` и `watchdog:` (см. graph.rs).
    //
    // `board` уезжает сюда целиком — это объявленный графом `boot:`-объект,
    // из которого его инициализаторы забирают объекты частичными move.
    // Такие заборы уже есть — сторож в блоке `watchdog:`{% if ota == "true" %}, вход узла
    // OTA в `fragments:` (`board.firmware`){% endif %} — и здесь же
    // окажутся ваши: `resources: [SLOT: T = board.<поле>]` вместо
    // `provide_<slot>(..)` перед этой строкой. Оттого и последняя строка
    // `spawn_all`: после неё `board` принадлежит прологу.
    //
    // Аппаратный сторож запускается ВНУТРИ этого вызова — инициализатором
    // блока `watchdog:` (docs/watchdog.md); всё своё долгое
    // ({% if ota == "true" %}проверка подписи, {% endif %}запись во flash) держите ВЫШЕ этой строки.{% if ota == "true" %} В release сторож
    // к этой строке уже тикает — его запустил bootloader, и путь от прыжка
    // сюда обязан уложиться в `bsp::wdg::HW_TIMEOUT_US`.{% endif %}
    //
    // Паника, а не `let _ =`, и это не педантизм: сторож запускается в
    // прологе, ДО спавна узлов, поэтому отказ (узел уже занят) оставляет
    // железо тикающим, а кормить его некому — проглоти вы `Err`, плата ушла
    // бы в сброс через `bsp::wdg::HW_TIMEOUT_US` без единой строки о причине.
    // Через `Display2Format`, а не `expect`: `Debug` у ошибки графа нет —
    // `supervisor` убрал его ради флеша, имя узла и вид отказа даёт `Display`.
    if let Err(err) = crate::graph::spawn_all(&spawner, board) {
        defmt::panic!("app: spawn_all: {}", defmt::Display2Format(&err));
    }
{%- else %}

    // Узлы спавнит `tasks::spawn_all` — по модулю на задачу, — а `board`
    // уезжает туда целиком.
    crate::tasks::spawn_all(spawner, board);
{%- endif %}
}
