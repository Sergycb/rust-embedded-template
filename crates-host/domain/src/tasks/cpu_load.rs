//! Узел `CPU_LOAD` графа задач: во сколько процентов занят процессор.
//!
//! Мерить это на МК нечем: счётчика инструкций нет, `/proc` нет, окна, в
//! котором можно посмотреть, тоже нет. Остаётся единственное, что доступно
//! исполнителю всегда, — время: сколько тиков прошло и сколько из них
//! микроконтроллер стоял. Всё остальное — работа.
//!
//! **Считает не этот узел, а цикл исполнителя.** Счётчик [`SleepTicks`] живёт
//! в `crates-cross/app/src/cpu_load.rs`, а пополняет его ручной цикл в
//! `main.rs`: тот спит в `WFE` и кладёт в счётчик ровно то, сколько тиков
//! проспал. Отсюда и [`SleepTicks`] в `Inputs` — узел его только читает, и
//! то через слот ресурса, который переживает перезапуск задачи.
//!
//! Почему цикл в `main` написан руками, а не `#[embassy_executor::main]`:
//! у того `WFE` живёт внутри `Executor::run`, между двумя вызовами `poll`, и
//! снаружи их не видно. А именно там проходит всё время, когда задач нет.
//! Считать сон как «всё, что не сон», — значит получить 100% на пустой плате.
//!
//! Два входа — [`task`] для проекта с графом и [`task_standalone`] для проекта
//! без него. Различаются они не вычислением (оно одно, [`CpuLoad::sample`]
//! над [`snapshot`]), а тем, куда уходит результат: в графе он публикуется в
//! `Watch`, без графа публиковать некому, и значение уходит в лог. Узел графа
//! — автомат `#[fsm::typestate]` на контексте [`CpuLoadCtx`], который эмитит
//! фрагмент ниже; `task_standalone` — не узел, контекста у него нет, и он
//! остаётся простым циклом. Выбор между ними делает
//! `crates-cross/app/src/main.rs`, потому что Liquid в `domain` запрещён.
//!
//! Публикация — только половина задачи. Кто и зачем читает загрузку, решает
//! проект: узел объявляет поле `PERCENT`, а подписчика добавляют в графе
//! (`subscribe:`), см. `docs/modules/app-graph.md`.

use core::sync::atomic::Ordering;

use defmt_or_log::info;
use embassy_time::{Duration, Instant, Timer};
use portable_atomic::AtomicU64;
use supervisor::policy::{BackoffPolicy, JitterPolicy};
use supervisor::runtime::TaskExit;

use crate::services::cpu_load::{CpuLoad, Sample};

/// Как часто снимаются счётчики.
///
/// Заодно период отметок у сторожа узла: замерение дольше, чем отметка, и
/// узел выглядел бы зависшим. Второе следствие — разрешение самой оценки:
/// усреднение идёт по [`SAMPLE_INTERVAL`], и профиль с характерным временем
/// меньше него в эти проценты не попадёт.
const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);

/// Потолок паузы между перезапусками узла.
///
/// Отдельная константа не ради читаемости, а потому что от неё зависит
/// [`WATCHDOG`] и связь эту держит `const`-assert ниже.
const BACKOFF_MAX: Duration = Duration::from_millis(500);

/// Политика пауз между перезапусками узла — те же цифры, что у
/// [`app::BACKOFF`](crate::app::BACKOFF), но свой потолок: у задачи, которая
/// меряет, а не ждёт, нет смысла держать паузу в секунды — нечего
/// перезапускать дольше, чем на цикл измерения.
pub const BACKOFF: BackoffPolicy = BackoffPolicy {
    first: Duration::from_millis(50),
    factor: 2,
    jitter: JitterPolicy::None,
    floor: Duration::from_millis(50),
    max: BACKOFF_MAX,
};

/// Сколько узлу позволено не отмечаться, прежде чем это будет замечено.
///
/// Считается от [`BACKOFF_MAX`], а не от [`SAMPLE_INTERVAL`]: слот сторожа граф
/// взводит один раз и снимает только когда узел уходит насовсем, а во время
/// паузы между перезапусками слот жив и кормить его некому. Та же поправка,
/// что и у [`app::WATCHDOG`](crate::app::WATCHDOG), и то же левое звено
/// цепочки таймаутов — `const`-assert ниже. Второе, с другого конца, не здесь:
/// `WATCHDOG + WATCHDOG_CHECK_EVERY < bsp::wdg::HW_TIMEOUT_US`
/// (`docs/watchdog.md`) — его держит `const`-assert в `app/src/graph.rs`, где
/// видны оба конца.
pub const WATCHDOG: Duration = Duration::from_secs(2);

// Левое звено цепочки таймаутов, проверенное компилятором: `<` на `Duration`
// в `const` не работает (нет const-трейтов), поэтому сравниваются тики.
const _: () = assert!(
    BACKOFF.max.as_ticks() < WATCHDOG.as_ticks(),
    "таймаут узла CPU_LOAD обязан быть больше потолка backoff'а: слот сторожа переживает паузу между \
     перезапусками, и кормить его в это время некому"
);

/// Счётчик тиков, проведённых исполнителем во сне.
///
/// Тик — это тик `embassy-time` (его `Instant::as_ticks`), а не цикл CPU:
/// на стендовом стенде разные частоты дают разные числа при одной и той же
/// загрузке, и это не расходится, это разные единицы.
///
/// Не [`core::sync::atomic::AtomicU64`] потому, что на Cortex-M0/M0+ его нет
/// вовсе, а [`portable_atomic`] — ровно про это (см. `docs/conventions.md`).
pub type SleepTicks = AtomicU64;

/// Что узлу нужно на входе. Собирает compose-site из того, что знает
/// приложение (счётчик лежит в `crates-cross/app/src/cpu_load.rs`).
///
/// Тип объявлен здесь, а не назван графом, по правилу фрагментов
/// `supervisor`: подсистема говорит, что ей нужно, приложение это
/// конструирует — зависимость направлена от приложения к подсистеме и никогда
/// наоборот.
pub struct Inputs {
    /// Счётчик тиков сна, который ведёт цикл исполнителя.
    pub sleep_ticks: &'static SleepTicks,
}

/// Снимок обоих счётчиков.
///
/// Оба читаются здесь, в одном месте и максимально близко друг к другу:
/// разъехавшиеся на миллисекунду чтения дали бы загрузку, которой не было.
fn snapshot(sleep_ticks: &SleepTicks) -> Sample {
    Sample {
        total_ticks: Instant::now().as_ticks(),
        sleep_ticks: sleep_ticks.load(Ordering::Relaxed),
    }
}

/// Узел `CPU_LOAD` графа: раз в [`SAMPLE_INTERVAL`] публикует занятость
/// процессора в процентах.
///
/// `ctx.percent` — отправитель, который граф создал из
/// `publish: [PERCENT: u8; 1]` во фрагменте ниже. `Watch` хранит последнее
/// значение, так что подписчик получит и пропущенные интервалы, лишь показав
/// последний из них: счётчик знает «сколько сейчас», а не «сколько было когда».
///
/// У автомата нет выходного состояния, поэтому [`TaskExit`] недостижим; у
/// задачи, которая может закончиться, `Completed` против `Failed` решает,
/// перезапустят её или нет.
pub async fn task(mut ctx: CpuLoadCtx<'_>) -> TaskExit {
    match ctx.run().await {}
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
    /// сторожил бы исполнитель, а не работу (см. `domain::app`).
    ///
    /// Первый снимок уходит в никуда (сравнивать не с чем) — так и задумано,
    /// см. [`CpuLoad::sample`].
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

/// Тот же замер для проекта без графа задач: спавнится из `main.rs` (через
/// `#[embassy_executor::task]`, потому что `Spawner::spawn` берёт токен, а не
/// функцию), а значение уходит в лог — публиковать ему некуда.
///
/// Не автомат: это не узел графа, контекста у него нет, а одно состояние с
/// тактом — ровно этот цикл.
///
/// Строка на каждом интервале — сознательное исключение из правила «пустой узел
/// молчит»: здесь она и есть результат измерения, и в проекте без графа иначе
/// негде его увидеть. Подпишетесь на узел и сможете убрать.
pub async fn task_standalone(sleep_ticks: &SleepTicks) {
    let mut cpu_load = CpuLoad::new();
    loop {
        Timer::after(SAMPLE_INTERVAL).await;
        if let Some(percent) = cpu_load.sample(snapshot(sleep_ticks)) {
            info!("domain: узел CPU_LOAD: загрузка {}%", percent);
        }
    }
}

// Узел объявлен здесь же, где лежит его задача, — как `APP_FRAG` в
// `domain::app`; compose-site (`crates-cross/app/src/graph.rs`) только
// перечисляет фрагмент, называет узел (`as CPU_LOAD`) и кормит его входом.
//
// `ctx:` — по той же причине, что в `domain::app`: контекст [`CpuLoadCtx`]
// эмитит фрагмент здесь, в `domain`, а не compose-site, и автомат стоит на нём
// рядом с узлом. Поле слота в нём — `&mut &'static SleepTicks`, поле
// публикации — `watch::Sender` из `sync-unsized`.
//
// `boot: inputs:` — собственная привязка фрагмента: compose-site пишет
// `fragments: [::domain::CPU_LOAD_FRAG as CPU_LOAD = ::domain::tasks::cpu_load::Inputs { sleep_ticks: … }]`,
// а `spawn_all` связывает её первой строкой пролога, и инициализатор слота ниже
// читает её поле. Так фрагмент не видит `Board` вовсе — только то, что ему
// дали, и счётчик не может «забыться» при заполнении: без него узел не
// собрался бы.
//
// `SLEEP_TICKS` — ресурсный слот, а не `shared:`, потому что значение пришло
// извне и принадлежит не узлу, а циклу исполнителя: слот отдаёт его на время
// прогона и забирает назад, `shared` просто делил бы ссылку. `&'static` в
// типе слота — ровно то, что нужно, чтобы ссылка пережила и создание
// статика, и паузу между перезапусками.
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
    boot: inputs: $crate::tasks::cpu_load::Inputs;

    node deps: [], restart: ::supervisor::policy::RestartPolicy::OnFailure,
        backoff: $crate::tasks::cpu_load::BACKOFF,
        resources: [SLEEP_TICKS: &'static $crate::tasks::cpu_load::SleepTicks = inputs.sleep_ticks],
        publish: [PERCENT: u8; 1],
        watchdog: $crate::tasks::cpu_load::WATCHDOG observe,
        ctx: $crate::tasks::cpu_load::CpuLoadCtx,
        task: $crate::tasks::cpu_load::task;
}
