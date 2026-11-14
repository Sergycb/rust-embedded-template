//! Период замера, счётчик тиков сна и снимок обоих счётчиков: то, что узел
// {%- if graph == "true" %}
//! `CPU_LOAD` читает раз в интервал, и его таймаут у сторожа.
// {%- else %}
//! `CPU_LOAD` читает раз в интервал, и цикл замера без графа — `log_load`.
// {%- endif %}

use core::sync::atomic::Ordering;

use embassy_time::{Duration, Instant};
use portable_atomic::AtomicU32;

// {%- if graph != "true" %}
use super::load::CpuLoad;
// {%- endif %}
use super::load::Sample;

// {%- if graph == "true" %}
/// Сколько узлу позволено не отмечаться, прежде чем это будет замечено.
///
/// Узел не падает и не перезапускается (у него нет `restart:`): самый длинный
/// законный промежуток между отметками — `SAMPLE_INTERVAL` плюс занятость
/// исполнителя, и 7 с — тот же запас, что у
/// [`heartbeat::WATCHDOG`](crate::heartbeat::WATCHDOG): стирание крупного
/// сектора флеша при приёме образа блокирует исполнитель на секунды. Нижнюю
/// границу — больше интервала — держит `const`-assert ниже. Второе звено, с
/// другого конца, не здесь:
/// `WATCHDOG + WATCHDOG_CHECK_EVERY < bsp::wdg::HW_TIMEOUT_MIN_US`
/// (`docs/watchdog.md`) — его держит `const`-assert в `app/src/graph.rs`, где
/// видны оба конца.
pub const WATCHDOG: Duration = Duration::from_secs(7);
// {%- endif %}

/// Как часто снимаются счётчики.
///
// {%- if graph == "true" %}
/// Заодно это период отметок у сторожа узла: отметка ставится на каждом
/// замере. Второе следствие — разрешение самой оценки:
// {%- else %}
/// Отсюда и разрешение самой оценки:
// {%- endif %}
/// усреднение идёт по `SAMPLE_INTERVAL`, и профиль с характерным временем
/// меньше него в эти проценты не попадёт.
pub(super) const SAMPLE_INTERVAL: Duration = Duration::from_millis(500);
// {%- if graph == "true" %}

// Левое звено цепочки таймаутов, проверенное компилятором: `<` на `Duration`
// в `const` не работает (нет const-трейтов), поэтому сравниваются тики.
const _: () = assert!(
    SAMPLE_INTERVAL.as_ticks() < WATCHDOG.as_ticks(),
    "таймаут узла CPU_LOAD обязан быть больше периода его отметок"
);
// {%- endif %}

/// Счётчик тиков, проведённых исполнителем во сне.
///
/// Тик — тик `embassy-time` (`Instant::as_ticks`), а не цикл CPU. 32 бита
/// хватает: загрузку дают не значения, а прирост между снимками по модулю 2³²
/// ([`CpuLoad`]). Почему 32 бита и
/// [`portable_atomic`], а не [`core::sync::atomic::AtomicU32`], —
/// `docs/modules/app-cpu-load.md`, «Почему `portable_atomic::AtomicU32`».
pub type SleepTicks = AtomicU32;

/// Тики, проведённые во сне. Пишет [`run`] (цикл исполнителя), читает узел
/// `CPU_LOAD` через [`sleep_ticks`], и ничто больше — отсюда и `Relaxed`:
/// значение нужно как грубая прикидка «сколько стояли», а порядок замеров
/// задаёт сам периодический тик.
static SLEEP_TICKS: SleepTicks = SleepTicks::new(0);

/// Записать, сколько тиков цикл исполнителя проспал в `WFE`.
///
/// Счётчик 32-битный и перематывается штатно: `fetch_add` складывает по
/// модулю 2³², и усечение `delta_ticks` — то же сложение по модулю. Узел
/// `CPU_LOAD` считает прирост между снимками так же
/// ([`load`](super::load::CpuLoad)).
pub fn record_sleep_ticks(delta_ticks: u64) {
    #[expect(
        clippy::cast_possible_truncation,
        reason = "счётчик по модулю 2³²: усечение и есть сложение по модулю"
    )]
    SLEEP_TICKS.fetch_add(delta_ticks as u32, Ordering::Relaxed);
}

/// Счётчик, который читает узел `CPU_LOAD`.
///
// {%- if graph == "true" %}
/// `'static` — потому что узел берёт его ресурсным слотом графа, а слот
/// живёт в `static`.
// {%- else %}
/// `'static` — потому что ссылку уносит задача замера, а аргументы задач
/// обязаны быть `'static`.
// {%- endif %}
#[must_use]
pub fn sleep_ticks() -> &'static SleepTicks {
    &SLEEP_TICKS
}

/// Ручной цикл исполнителя: поллить, ждать, пока нечего делать, и отмечать в
/// счётчике сна, сколько тиков проспали.
///
/// Бесконечный: вызывается из `main` бинарника вместо `Executor::run`,
/// единственная причина, по которой `main` не `#[embassy_executor::main]`, —
/// замер сна (docs/modules/app-cpu-load.md, «Почему цикл исполнителя в `main`
/// написан руками»). `wait` — ожидание события (`WFE` у прошивки); цикл ждёт
/// ровно там. `poll` — первым: часы, которыми меряется сон, поднимает
/// `Board::new` в первом же проходе (`embassy_stm32::init`), и до него
/// `Instant` читать нечем.
///
/// # Safety
///
/// Обязанности те же, что у `Executor::run`: поллить исполнитель можно только
/// из единственного места, и это оно.
pub fn run(executor: &'static embassy_executor::raw::Executor, wait: impl Fn()) -> ! {
    loop {
        // SAFETY: этот цикл — единственный, кто поллит исполнитель; ровно то
        // обещание, которое даёт `Executor::run`.
        #[expect(unsafe_code, reason = "обоснование — в документации `run`")]
        unsafe {
            executor.poll();
        }
        let before = Instant::now().as_ticks();
        wait();
        record_sleep_ticks(Instant::now().as_ticks().saturating_sub(before));
    }
}

// {%- if graph != "true" %}
/// Замер загрузки: спавнится из `tasks::spawn_all` в `app` (через
/// `#[embassy_executor::task]`, потому что `Spawner::spawn` берёт токен, а не
/// функцию), а значение уходит в лог — публиковать ему некуда.
///
/// Не автомат: контекста у него нет, а одно состояние с тактом — ровно этот
/// цикл.
///
/// Строка на каждом интервале — сознательное исключение из правила «пустой узел
/// молчит»: здесь она и есть результат измерения, и иначе его негде увидеть.
/// Понадобится значение в коде — уберёте.
pub async fn log_load(sleep_ticks: &SleepTicks) {
    let mut cpu_load = CpuLoad::new();
    loop {
        embassy_time::Timer::after(SAMPLE_INTERVAL).await;
        if let Some(percent) = cpu_load.sample(snapshot(sleep_ticks)) {
            defmt_or_log::info!("cpu_load: загрузка {}%", percent);
        }
    }
}
// {%- endif %}

/// Снимок обоих счётчиков.
///
/// Оба читаются здесь, в одном месте и максимально близко друг к другу:
/// разъехавшиеся на миллисекунду чтения дали бы загрузку, которой не было.
pub(super) fn snapshot(sleep_ticks: &SleepTicks) -> Sample {
    Sample {
        total_ticks: Instant::now().as_ticks(),
        sleep_ticks: sleep_ticks.load(Ordering::Relaxed),
    }
}
