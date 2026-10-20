#![no_std]
#![no_main]
#![allow(
    missing_debug_implementations,
    reason = "бинарник ничего не экспортирует, а shadow-rs генерирует pub-типы без Debug"
)]

use shadow_rs::shadow;
shadow!(build);
mod cpu_load;
{%- if graph == "true" %}
mod graph;
{%- endif %}
{%- if signed == "true" %}
mod image;
{%- endif %}
{%- if protocol_link == "true" %}
mod link;
{%- endif %}
{%- if graph != "true" %}
mod tasks;
{%- endif %}

use defmt::info;
// RTT остаётся дефолтным транспортом в обоих профилях — сырой шаблон не знает,
// какой конкретный USART на плате пользователь отведёт под лог без пробника.
// Паттерн замены (UART, без RTT/пробника) для release — в
// docs/modules/app-graph.md, подключается по мере готовности платы (Board
// должен будет отдавать объект транспорта, собранный из реальной периферии).
use defmt_rtt as _;

use embassy_executor::raw::Executor;
use embassy_time::Instant;
use static_cell::StaticCell;

/// Исполнитель задач — свой, а не из `#[embassy_executor::main]` (почему — в
/// `main`). `StaticCell`, а не `ConstStaticCell`: `Executor::new` не `const`.
static EXECUTOR: StaticCell<Executor> = StaticCell::new();

{%- if has_protobuf == "true" %}

/// Сведения об устройстве для ответа хосту на `info`: версия и коммит — из
/// `shadow-rs` (те же, что в баннере старта), остальное — с платы.
fn device_info(spec: bsp::ChipSpec) -> domain::host::DeviceInfo {
    domain::host::DeviceInfo {
        version: build::PKG_VERSION,
        commit: build::SHORT_COMMIT,
        chip: spec.chip,
        uid: spec.uid,
        flash_size: spec.flash_size,
        sys_hz: spec.sys_hz,
    }
}
{%- endif %}

#[bsp::cpu::entry]
fn main() -> ! {
    // Первая же строка лога отвечает на вопрос «а что вообще залито в плату»:
    // версия пакета и коммит, из которого собран образ (их подставляет
    // `shadow-rs` в build.rs). Без этого build-info собиралась бы впустую, а по
    // OTA легко получить плату с прошивкой, происхождение которой неизвестно.
    info!(
        "app: starting {} ({})",
        build::PKG_VERSION,
        build::SHORT_COMMIT,
    );

    // Причина прошлого падения, если оно было: паникёр release-профиля успел
    // записать её в регион PANIC, а тот пережил сброс (RAM, но вне секций,
    // которые cortex-m-rt инициализирует при старте). Читается один раз —
    // `panic-persist` стирает свою магию, — поэтому печатать надо сразу и до
    // всего остального: следующий сброс эту строку уже не покажет.
    //
    // ДО `Board::new`, и это важно: тот умеет паниковать сам (негодная
    // конфигурация тактирования, негодный раздел `CONFIG`), а в release
    // паника это сброс. Печатай мы после него — устройство, падающее внутри
    // `new`, крутилось бы в цикле перезагрузок молча.
    //
    // Только release: в dev паникёр — `panic-probe`, дамп никто не пишет, и
    // читать было бы нечего (см. `bsp::panic`).
    #[cfg(not(debug_assertions))]
    if let Some(reason) = bsp::panic::previous() {
        defmt::error!("app: предыдущий запуск упал: {}", reason);
    }

    let board = bsp::Board::new();
    // Следом — на чём именно: чип и его заводской номер, по которому
    // экземпляр платы отличается от соседнего.
    info!("app: {}", board.spec);

    // Исполнитель — свой, а не `#[embassy_executor::main]`, и это единственная
    // причина, по которой `main` здесь синхронный. У макроса `WFE` живёт
    // внутри `Executor::run`, между двумя вызовами `poll`, и снаружи их не
    // видно — а именно там проходит всё время, когда задач нет. Измерению
    // загрузки нужно знать, сколько тиков микроконтроллер стоял, и кроме
    // явного `WFE` этой величины взять неоткуда. Подробности — в модуле
    // `cpu_load` (docs/modules/app-cpu-load.md).
    //
    // `usize::MAX` вместо нуля — тот же «контекст», что и у макроса: на
    // thread-режиме `__pender` делает `SEV` (пока `executor-interrupt` не
    // включён, ровно так же), а не `WFE`, то есть пробуждение задач не теряет.
    // Цикл ниже ставит `WFE` сам, и всё, что остаётся на долю pender'а, — это
    // возврат из него, а не сон.
    let executor = EXECUTOR.init(Executor::new(usize::MAX as *mut ()));
    let spawner = executor.spawner();
{%- if graph == "true" %}

    // Граф задач — единственное место, где в этом проекте появляются задачи.
    // Голый `spawner.spawn(..)` рядом с ним завёл бы вторую, неуправляемую
    // ветку: без `restart:`, `deps:` и `watchdog:` (см. graph.rs).
    //
    // `board` уезжает сюда целиком — это объявленный графом `boot:`-объект,
    // из которого его инициализаторы забирают объекты частичными move.
    // Такие заборы уже есть — сторож в блоке `watchdog:`{% if ota == "true" %}, вход узла
    // OTA в `fragments:` (`board.ota`){% endif %} — и здесь же
    // окажутся ваши: `resources: [SLOT: T = board.<поле>]` вместо
    // `provide_<slot>(..)` перед этой строкой. Оттого и последняя строка
    // `spawn_all`: после неё `board` принадлежит прологу.
    //
    // Аппаратный сторож запускается ВНУТРИ этого вызова — инициализатором
    // блока `watchdog:`, — а не в `Board::new` и не строкой выше: с этого
    // момента отсчёт идёт, а кормить начнёт только тикер графа, и чем короче
    // промежуток между этими двумя моментами, тем меньше шансов уйти в
    // перезагрузки молча. В прологе там остаётся только спавн узлов; всё своё
    // долгое (проверка подписи, запись во flash) держите ВЫШЕ этой строки.{% if ota == "true" %}
    //
    // Но в release сторож к этой строке уже тикает: его запустил bootloader
    // (на той же цифре `bsp::wdg::HW_TIMEOUT_US`, 10 с), и весь
    // путь от прыжка сюда — `Board::new` и ваше долгое —
    // обязан в него уложиться (docs/watchdog.md).{% endif %}
    //
    // Паника, а не `let _ =`, и это не педантизм: сторож запускается в
    // прологе, ДО спавна узлов, поэтому отказ (узел уже занят) оставляет
    // железо тикающим, а кормить его некому — проглоти вы `Err`, плата ушла
    // бы в сброс через `bsp::wdg::HW_TIMEOUT_US` без единой строки о причине.
    // Через `Display2Format`, а не `expect`: `Debug` у ошибки графа нет —
    // `supervisor` убрал его ради флеша, имя узла и вид отказа даёт `Display`.
    if let Err(err) = graph::spawn_all(&spawner, board) {
        defmt::panic!("app: spawn_all: {}", defmt::Display2Format(&err));
    }
{%- else %}

    // Графа нет: узлы спавнит `tasks::spawn_all` — по модулю на задачу, как
    // фрагменты графа, — а `board` уезжает туда целиком.
    tasks::spawn_all(spawner, board);
{%- endif %}

    // Цикл исполнителя: спим, пока нечего делать, и отмечаем в счётчике, сколько
    // тиков проспали. `wfe()` возвращается и по событию (SEV от `spawn`, от
    // пробуждения задачи), и по таймеру — от того, что сработал следующий
    // `Timer` задачи, — так что это единственное место, где цикл ждёт.
    // `poll` — `unsafe`, и это то же обещание, что у `Executor::run`: поллить
    // исполнитель можно только из единственного места, а оно здесь.
    loop {
        let before = Instant::now().as_ticks();
        bsp::cpu::wait_for_event();
        cpu_load::record_sleep_ticks(Instant::now().as_ticks().saturating_sub(before));
        // SAFETY: поллить исполнитель можно только из единственного места, а
        // оно здесь — цикл ниже единственный, кто его крутит, и `spawn_all` к
        // этому моменту уже отработал. Ровно то же обещание, что у `Executor::run`.
        //
        // `#[expect(unsafe_code)]` — вместе с остальными в `crates-cross` (см. `crates-cross/Cargo.toml`)
        // (`boot/src/main.rs`, `app/src/image.rs`): unsafe в прошивке допустим, но каждое
        // место обязано быть перечислено и объяснено, а линт `unsafe_code`
        // заставляет перечислять.
        #[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
        unsafe {
            executor.poll();
        }
    }
}
