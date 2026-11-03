//! Рабочий образ `app` — запуск прошивки: ловушки, баннер, причины сброса и
//! падения, затем исполнитель и его ручной цикл с замером сна
//! (docs/modules/app-cpu-load.md); задача `start` собирает плату и спавнит
//! узлы. Всё, что не запуск, — библиотека пакета (`src/lib.rs`).
// {%- if test_image == "true" %}
//! Запуск тестового образа — свой, в `src/bin/app-unconfirmed.rs`, и повторяет
//! этот шаг в шаг: правите запуск здесь (свой слот перед `spawn_all`, новый шаг
//! до исполнителя) — правьте и там, иначе `cargo xtask test host-target`
//! проверит обмен на другой прошивке.
// {%- endif %}
#![no_std]
#![no_main]

use defmt::{error, info};
use embassy_executor::Spawner;
use embassy_executor::raw::Executor;
use embassy_time::Instant;
use static_cell::StaticCell;
{%- if ota == "true" %}

/// Свойства этого образа (`app::version`): версия — `version` из Cargo.toml;
/// `confirmed: false` — узел OTA подтвердит образ сам (`mark_booted`).
const IMAGE: app::version::Image = app::version::Image {
    version: app::version::PACKAGE_VERSION,
    text: app::build::PKG_VERSION,
    confirmed: false,
};

/// Версия этого образа, видимая снаружи по имени символа.
///
/// Существует ради одной вещи: `cargo xtask build` находит её в ELF и
/// дописывает те же четыре байта в хвост `app.bin`. Так у версии остаётся один
/// источник — то, что скомпилировано в прошивку, — и хост не может приписать
/// образу чужой номер, прочитав его из другого места.
///
/// `#[used]` и `no_mangle` вдвоём: первое просит оставить статик, который код
/// не читает, второе — сохранить имя, по которому его ищет xtask. Строго
/// говоря, `#[used]` обязывает только компилятор (rust-lang/rust#47384), но
/// LLVM ставит символу `SHF_GNU_RETAIN`, и в release он на месте; пропади он —
/// `cargo xtask build` не найдёт символ и остановит сборку.
#[used]
#[expect(unsafe_code, reason = "обоснование — в комментарии выше")] // имя символа нужно xtask, см. выше
#[unsafe(no_mangle)]
pub static FW_VERSION_IN_IMAGE: u32 = IMAGE.version;
{%- endif %}

/// Исполнитель задач — свой, а не из `#[embassy_executor::main]` (почему — в
/// [`main`]). `StaticCell`, а не `ConstStaticCell`: `Executor::new` не `const`.
static EXECUTOR: StaticCell<Executor> = StaticCell::new();

/// Запуск: ловушки, баннер, причины сброса и падения, затем исполнитель и
/// его цикл.
///
/// # Panics
///
/// Задача `start` уже заспавнена — `main` позвали второй раз.
#[cortex_m_rt::entry]
fn main() -> ! {
{%- if core_baseline != "true" %}
    // Первым делом: ловушка деления действует на всё, что выполнится дальше,
    // включая `Board::new` (см. `app::fault`).
    app::fault::enable_div_0_trap();
{%- endif %}
    // Первая же строка лога отвечает на вопрос «а что вообще залито в плату»:
    // версия пакета и коммит, из которого собран образ (их подставляет
    // `shadow-rs` в build.rs). Без этого build-info собиралась бы впустую, а по
    // OTA легко получить плату с прошивкой, происхождение которой неизвестно.
{%- if ota == "true" %}
    info!("app: запуск {} ({})", IMAGE.text, app::build::SHORT_COMMIT);
{%- else %}
    info!(
        "app: запуск {} ({})",
        app::build::PKG_VERSION,
        app::build::SHORT_COMMIT
    );
{%- endif %}

    // Причина сброса — до `Board::new` и до всего, что может повиснуть или
    // упасть (docs/diagnostics.md, «Причина сброса»). Сброс из `ensure_*`
    // в `Board::new` ставит свой флаг заново — его прочитает следующий старт.
    let reset = bsp::reset::take_cause();
    info!("app: причина сброса {}", reset);
{%- if has_protobuf == "true" or net_services != "" or has_cli == "true" %}
    app::device::set_reset(reset);
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
    if let Some(reason) = app::panic::take() {
        error!("app: предыдущий запуск упал: {}", reason);
    }

    // Исполнитель — свой, а не `#[embassy_executor::main]`, и это единственная
    // причина, по которой `main` здесь синхронный: только так измеряется сон
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
        app::cpu_load::record_sleep_ticks(Instant::now().as_ticks().saturating_sub(before));
    }
}

/// Первая задача: собирает плату и спавнит узлы.
///
/// Задачей, а не в [`main`] до исполнителя: {% if external_flash == "true" %}`Board::new` ждёт таймерами
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
    // ветку: без `deps:` и `watchdog:` (см. `src/graph.rs`).
    //
    // `board` уезжает сюда целиком{% if ota == "true" %} — вместе со свойствами образа, в
    // `boot:`-объекте графа (`app::Boot`){% else %} — это объявленный графом `boot:`-объект{% endif %},
    // из которого его инициализаторы забирают объекты частичными move.
    // Такие заборы уже есть — сторож в блоке `watchdog:`{% if ota == "true" %}, вход узла
    // OTA в `fragments:` (`boot.board.firmware` и `boot.image`){% endif %} — и здесь же
    // окажутся ваши: привязка фрагмента в `fragments:` (`= …Wiring { поле:
    // {% if ota == "true" %}boot.{% endif %}board.<поле> }`, docs/modules/app-graph.md) вместо
    // `provide_<slot>(..)` перед этой строкой. Оттого и последняя строка
    // `spawn_all`: после неё `board` принадлежит прологу.
    //
    // Аппаратный сторож запускается ВНУТРИ этого вызова — инициализатором
    // блока `watchdog:` (docs/watchdog.md); всё своё долгое
    // (запись во flash) держите ВЫШЕ этой строки.{% if ota == "true" %} В release сторож
    // к этой строке уже тикает — его запустил bootloader, и путь от прыжка
    // сюда обязан уложиться в `bsp::wdg::HW_TIMEOUT_MIN_US` (таймаут сторожа
    // при самом быстром LSI).{% endif %}
    //
    // Паника, а не `let _ =`, и это не педантизм: сторож запускается в
    // прологе, ДО спавна узлов, поэтому отказ (узел уже занят) оставляет
    // железо тикающим, а кормить его некому — проглоти вы `Err`, плата ушла
    // бы в сброс через `bsp::wdg::HW_TIMEOUT_US` без единой строки о причине.
    // Через `Display2Format`, а не `expect`: `Debug` у ошибки графа нет —
    // `supervisor` убрал его ради флеша, имя узла и вид отказа даёт `Display`.
{%- if ota == "true" %}
    let boot = app::Boot {
        board,
        image: IMAGE,
    };
    if let Err(err) = app::spawn_all(&spawner, boot) {
{%- else %}
    if let Err(err) = app::spawn_all(&spawner, board) {
{%- endif %}
        defmt::panic!("app: spawn_all: {}", defmt::Display2Format(&err));
    }
{%- else %}

    // Узлы спавнит `tasks::spawn_all` — по модулю на задачу, — а `board`
    // уезжает туда целиком{% if ota == "true" %}, со свойствами образа рядом{% endif %}.
    app::spawn_all(spawner, board{% if ota == "true" %}, IMAGE{% endif %});
{%- endif %}
}
