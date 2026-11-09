//! Сетка периодических тактов и deadline для гонок с источниками событий.
//!
//! Две утилиты периодических циклов узлов. Обе — осознанные замены примитивам
//! `embassy-time`:
//!
//! - [`next_tick`](crate::tick::next_tick) вместо [`embassy_time::Ticker`]:
//!   отставший `Ticker` догоняет пачкой тактов подряд, а циклам узлов
//!   пропущенные такты не нужны — работа выполнилась бы N раз разом за один
//!   круг;
//! - [`deadline`](crate::tick::deadline) вместо [`embassy_time::Timer::at`] в
//!   гонке `select`: `Timer` на первом опросе всегда отвечает `Poll::Pending`,
//!   даже если срок уже прошёл, и готовый прямо сейчас источник обогнал бы
//!   наступивший срок: такт heartbeat откладывался бы потоком кадров, а
//!   просроченный приём OTA — пришедшей командой.
//!   [`deadline`](crate::tick::deadline) читает часы на каждом опросе, первом
//!   включённым: срок, наступивший до начала круга, выигрывает гонку у любого
//!   события.

use core::future::{Future, poll_fn};
use core::pin::Pin;
use core::task::Poll;

use embassy_time::{Duration, Instant, Timer};

/// Следующая точка сетки с шагом `period`, продолженной от `prev`: `prev +
/// period`, или `now`, если тот момент уже прошёл.
///
/// Опоздавший вызов получает один такт сразу, и сетка дальше идёт от `now`:
/// пропущенные такты отброшены, а не выданы пачкой подряд, как у
/// [`embassy_time::Ticker`]. Насыщается в [`Instant::MAX`].
#[must_use]
pub fn next_tick(prev: Instant, period: Duration, now: Instant) -> Instant {
    let next = prev.checked_add(period).unwrap_or(Instant::MAX);
    if next > now { next } else { now }
}

/// Future, готовая, когда наступит `at`, — в том числе на самом первом
/// опросе, если срок уже прошёл.
///
/// В отличие от [`embassy_time::Timer::at`], которая на первом опросе всегда
/// отвечает `Poll::Pending` (даже для прошедшего срока) и лишь регистрирует
/// waker, `deadline` читает часы на каждом опросе. Поэтому в гонке
/// `select(источник, deadline)` наступивший срок выигрывает у источника,
/// готового прямо сейчас, — а не ждёт лишнего опроса, как у `Timer`.
///
/// Cancel-safe: снятие забывает всё. Пока `at` впереди, каждый `Pending`
/// опрашивает свежий [`Timer`], который заново регистрирует waker.
pub fn deadline(at: Instant) -> impl Future<Output = ()> {
    poll_fn(move |cx| {
        if at <= Instant::now() {
            Poll::Ready(())
        } else {
            // Timer на этом опросе нужен только ради регистрации waker —
            // результат не важен: готовность решит проверка часов выше.
            let _polled = Pin::new(&mut Timer::at(at)).poll(cx);
            Poll::Pending
        }
    })
}
