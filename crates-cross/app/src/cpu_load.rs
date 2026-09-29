#![doc = include_str!("../../../docs/modules/app-cpu-load.md")]

use core::sync::atomic::Ordering;

use domain::tasks::cpu_load::SleepTicks;

/// Тики, проведённые во сне. Пишет цикл исполнителя в `main.rs`, читает узел
/// `CPU_LOAD`, и ничего больше — отсюда и `Relaxed`: значение нужно как грубая
/// прикидка «сколько стояли», а порядок замеров задаёт сам периодический тик.
static SLEEP_TICKS: SleepTicks = SleepTicks::new(0);

/// Записать, сколько тиков цикл исполнителя проспал в `WFE`.
///
/// Счётчик монотонный, и переполнение в `fetch_add` — единственное, чего тут
/// не бывает: перемотался бы `u64` тиков, а это сотни лет при любой частоте,
/// на которой плата вообще работает.
pub fn record_sleep_ticks(delta_ticks: u64) {
    SLEEP_TICKS.fetch_add(delta_ticks, Ordering::Relaxed);
}

/// Счётчик, который читает узел `CPU_LOAD`.
///
/// `'static` — потому что узел берёт его ресурсным слотом графа, а слот
/// живёт в `static`.
pub fn sleep_ticks() -> &'static SleepTicks {
    &SLEEP_TICKS
}
{%- if graph != "true" %}

// Графа нет — узел `CPU_LOAD` не на что подписан, и спавнить его берётся
// `main`. Обёртка нужна потому, что `Spawner::spawn` в `embassy-executor` 0.10
// берёт `SpawnToken`, а не функцию: `#[embassy_executor::task]` и есть тот
// способ получить токен, не заводя executor-обвязку в домене, где ей не
// место. Токен выдаёт она же, поэтому `expect` на её результате — в
// `main.rs`.
//
// Значение при этом по-прежнему уходит в лог, а не в `Watch`: канал без
// читателя — это `Watch` вхолостую, и держать ради него `embassy-sync` в
// приложении незачем. Свою подписку заводите сами, когда появится читатель.
#[embassy_executor::task]
pub async fn standalone_task() {
    domain::tasks::cpu_load::task_standalone(sleep_ticks()).await;
}
{%- endif %}
