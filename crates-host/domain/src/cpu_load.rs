//! Узел `CPU_LOAD`: во сколько процентов занят процессор.
//!
//! Мерить это на МК нечем: счётчика инструкций нет, `/proc` нет, окна, в
//! котором можно посмотреть, тоже нет. Остаётся единственное, что доступно
//! исполнителю всегда, — время: сколько тиков прошло и сколько из них
//! микроконтроллер стоял. Всё остальное — работа.
//!
//! **Считает не этот узел, а цикл исполнителя.** Счётчик `SleepTicks` живёт
//! в `crates-cross/app/src/cpu_load.rs`, а пополняет его ручной цикл в
//! `main.rs`: тот спит в `WFE` и кладёт в счётчик ровно то, сколько тиков
//! проспал. Отсюда и `SleepTicks` в `IdleMeter` — узел его только читает, и
//! то через слот ресурса, который переживает перезапуск задачи.
//!
//! Почему цикл в `main` написан руками, а не `#[embassy_executor::main]`:
//! у того `WFE` живёт внутри `Executor::run`, между двумя вызовами `poll`, и
//! снаружи их не видно. А именно там проходит всё время, когда задач нет.
//! Считать сон как «всё, что не сон», — значит получить 100% на пустой плате.
//!
// {%- if graph == "true" %}
//! Вход — `task`: замер (`CpuLoad::sample` над `snapshot`) публикуется в
//! `Watch`. Узел — автомат `#[fsm::typestate]` на контексте `CpuLoadCtx`,
//! который эмитит фрагмент ниже.
//!
//! Публикация — только половина задачи. Кто и зачем читает загрузку, решает
//! проект: узел объявляет поле `PERCENT`, а подписчика добавляют в графе
//! (`subscribe:`), см. `docs/modules/app-graph.md`.
// {%- else %}
//! Вход — `log_load`: замер (`CpuLoad::sample` над `snapshot`) уходит в лог —
//! публиковать его некому. Контекста у него нет, и он остаётся простым
//! циклом.
// {%- endif %}

/// Загрузка процессора по двум счётчикам тиков: чистая арифметика без
/// времени, железа и каналов — см. модуль.
pub mod load;

/// Период замера, счётчик сна и его снимок — то, на чём стоит замер,
/// см. модуль.
mod sampling;

pub use sampling::SleepTicks;
// {%- if graph == "true" %}
pub use sampling::WATCHDOG;
// {%- else %}
pub use sampling::log_load;
// {%- endif %}

// {%- if graph == "true" %}
use embassy_time::Instant;
use supervisor::runtime::TaskExit;

use load::CpuLoad;
use sampling::{SAMPLE_INTERVAL, snapshot};

// Вход узла — `IdleMeter`: структуру эмитит сам фрагмент (`boot: …
// struct`). Собирает её compose-site из того, что знает
// приложение (счётчик лежит в `crates-cross/app/src/cpu_load.rs`).
//
// Тип объявлен здесь, а не назван графом, по правилу фрагментов
// `supervisor`: подсистема говорит, что ей нужно, приложение это
// конструирует — зависимость направлена от приложения к подсистеме и никогда
// наоборот.
//
// Узел объявлен здесь же, где лежит его задача, — как `HEARTBEAT_FRAG` в
// `domain::heartbeat`; compose-site (`crates-cross/app/src/graph.rs`) только
// перечисляет фрагмент, называет узел (`as CPU_LOAD`) и кормит его входом.
//
// `ctx:` — по той же причине, что в `domain::heartbeat`: контекст [`CpuLoadCtx`]
// эмитит фрагмент здесь, в `domain`, а не compose-site, и автомат стоит на нём
// рядом с узлом. Поле слота в нём — `&mut &'static SleepTicks`, поле
// публикации — `watch::Sender` из `sync-unsized`.
//
// `boot: inputs:` — собственная привязка фрагмента: compose-site пишет
// `fragments: [::domain::CPU_LOAD_FRAG as CPU_LOAD = ::domain::cpu_load::IdleMeter { sleep_ticks: … }]`,
// а `spawn_all` связывает её первой строкой пролога, и инициализатор слота ниже
// читает её поле. Так фрагмент не видит `Board` вовсе — только то, что ему
// дали, и счётчик не может «забыться» при заполнении: без него узел не
// собрался бы.
//
// `SLEEP_TICKS` — ресурсный слот, а не `shared:`, потому что значение пришло
// извне и принадлежит не узлу, а циклу исполнителя: слот отдаёт его на время
// прогона и забирает назад, `shared` просто делил бы ссылку. `&'static` в
// типе слота — ровно то, что нужно, чтобы ссылка пережила создание
// статика.
//
// `PERCENT: u8; 1` — количество подписчиков задано явно, потому что в сыром
// шаблоне их ноль, а без числа макрос такое объявление не принимает. Один
// слот — это не «один подписчик», а «место для одного»: кто первым позовёт
// `cpu_load_percent_receiver()` в `graph.rs`, тот и займёт его.
//
// `watchdog:` — с отметкой после измерения, поэтому узел наблюдаем (`observe`):
// замерзающее измерение тише ненаблюдаемого, но не безопаснее, а сброса
// жалко. Без `observe` просрочка ушла бы в аппаратный сброс здоровой платы.
supervisor::supervisor_fragment! {
    name: CPU_LOAD_FRAG;
    boot: inputs: struct $crate::cpu_load::IdleMeter;

    node deps: [], restart: Never,
        resources: [
            /// Счётчик тиков сна, который ведёт цикл исполнителя.
            SLEEP_TICKS: &'static $crate::cpu_load::SleepTicks = inputs.sleep_ticks
        ],
        publish: [PERCENT: u8; 1],
        watchdog: $crate::cpu_load::WATCHDOG observe,
        ctx: $crate::cpu_load::CpuLoadCtx,
        task: $crate::cpu_load::task;
}

#[fsm::typestate]
impl CpuLoadCtx<'_> {
    fsm::machine! {
        machine Sampler mod edges {
            states {
                /// Ждёт замера в `next`; `load` помнит прошлый снимок.
                Waiting { next: Instant, load: CpuLoad },
            }
            start => begin -> Waiting;
            listen {
                Waiting { next, .. } => tick: fsm::deadline(*next),
            }
            transitions {
                Waiting + tick(()) => sample -> self,
            }
        }
    }

    /// Назначает первый замер; прошлого снимка ещё нет.
    #[expect(
        clippy::unused_self,
        reason = "сигнатуру действия задаёт таблица `#[fsm::typestate]`"
    )]
    fn begin(&mut self) -> Waiting {
        Waiting {
            next: Instant::now() + SAMPLE_INTERVAL,
            load: CpuLoad::new(),
        }
    }

    /// Отмечается у сторожа, снимает счётчики и публикует процент.
    ///
    /// Отметка — ПОСЛЕ пробуждения и до измерения: «узел жив» здесь значит
    /// ровно «узел дождался своего интервала», а не «future опрашивают».
    /// Автоматической отметки по факту опроса нет намеренно — иначе сторож
    /// сторожил бы исполнитель, а не работу (см. `domain::heartbeat`).
    ///
    /// Первый снимок уходит в никуда (сравнивать не с чем) — так и задумано,
    /// см. `CpuLoad::sample`.
    ///
    /// Следующий замер — `fsm::next_tick`, а не `Ticker`: отставший `Ticker`
    /// догоняет пачкой тиков подряд, и замеры через микросекунды
    /// опубликовали бы ложные 0% или 100% поверх честного значения.
    fn sample(&mut self, s: &mut Waiting) {
        self.heartbeat.feed();
        if let Some(percent) = s.load.sample(snapshot(self.sleep_ticks)) {
            self.percent.send(percent);
        }
        s.next = fsm::next_tick(s.next, SAMPLE_INTERVAL, Instant::now());
    }
}

/// Узел `CPU_LOAD` графа: раз в `SAMPLE_INTERVAL` публикует занятость
/// процессора в процентах.
///
/// `ctx.percent` — отправитель, который граф создал из
/// `publish: [PERCENT: u8; 1]` во фрагменте ниже. `Watch` хранит последнее
/// значение, так что подписчик получит и пропущенные интервалы, лишь показав
/// последний из них: счётчик знает «сколько сейчас», а не «сколько было когда».
///
/// У автомата нет выходного состояния, поэтому [`TaskExit`] недостижим: узел
/// не падает (`restart: Never`).
pub async fn task(mut ctx: CpuLoadCtx<'_>) -> TaskExit {
    match ctx.run().await {}
}
// {%- endif %}
