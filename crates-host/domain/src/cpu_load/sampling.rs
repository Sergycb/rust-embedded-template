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
/// Узел не падает и не перезапускается (`restart: Never`): самый длинный
/// законный промежуток между отметками — `SAMPLE_INTERVAL` плюс занятость
/// исполнителя, и 7 с — тот же запас, что у
/// [`heartbeat::WATCHDOG`](crate::heartbeat::WATCHDOG): стирание крупного
/// сектора флеша при приёме образа блокирует исполнитель на секунды. Нижнюю
/// границу — больше интервала — держит `const`-assert ниже. Второе звено, с
/// другого конца, не здесь:
/// `WATCHDOG + WATCHDOG_CHECK_EVERY < bsp::wdg::HW_TIMEOUT_US`
/// (`docs/watchdog.md`) — его держит `const`-assert в `app/src/graph.rs`, где
/// видны оба конца.
pub const WATCHDOG: Duration = Duration::from_secs(7);
// {%- endif %}

/// Как часто снимаются счётчики.
///
// {%- if graph == "true" %}
/// Заодно период отметок у сторожа узла: замерение дольше, чем отметка, и
/// узел выглядел бы зависшим. Второе следствие — разрешение самой оценки:
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
/// Тик — это тик `embassy-time` (его `Instant::as_ticks`), а не цикл CPU:
/// на стендовом стенде разные частоты дают разные числа при одной и той же
/// загрузке, и это не расходится, это разные единицы.
///
/// 32 бита, а не 64: 64-битных атомиков у Cortex-M нет, и `AtomicU64` там —
/// эмуляция `portable-atomic` с глобальной таблицей блокировок (два килобайта
/// оперативной памяти на M4/M7). Счётчику хватает и 32: загрузку дают не его значения, а прирост
/// между двумя снимками, и он считается по модулю 2³² ([`CpuLoad`](super::load::CpuLoad)) —
/// верно, пока между снимками меньше 2³² тиков (71 минута при тике 1 мегагерц,
/// а снимок — раз в полсекунды).
///
/// Не [`core::sync::atomic::AtomicU32`] потому, что на Cortex-M0/M0+ у него
/// нет `fetch_add` (нет CAS), а [`portable_atomic`] — ровно про это (см.
/// `docs/conventions.md`).
pub type SleepTicks = AtomicU32;

// {%- if graph != "true" %}
/// Замер загрузки: спавнится из `main.rs` (через
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
            defmt_or_log::info!("domain: узел CPU_LOAD: загрузка {}%", percent);
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
