//! Тесты узла `HEARTBEAT` — и образец теста своего узла.
//!
//! Граф для теста узла не нужен: `supervisor::stand!` строит контекст
//! фрагмента и каждый его порт отдельным полем стенда, а время двигает
//! `MockDriver` — по команде теста, а не по часам. Узел опрашивается руками:
//! `poll` — один шаг, `advance` — время, за которое узлу есть что сделать.
//!
//! Когда в узле появится работа, тест растёт отсюда же: то, что узел
//! публикует (`publish:`), читается через `stand.<поле>.receiver()`, то, что он
//! получает (`inbox:`), — пишется через `stand.<поле>.sender()`, ресурсы
//! (`resources:`) приходят аргументами `stand.ctx(..)`. Подробно — книга
//! `rust-lib`, глава «Testing a node».

use core::pin::pin;
use core::task::{Context, Waker};

use embassy_time::MockDriver;

use super::task;
use super::timing::IDLE_PERIOD;

/// Узел живёт, сколько бы тактов ни прошло: не выходит (у автомата нет
/// выходного состояния) и между тактами ждёт, а не крутится — иначе
/// `poll` не вернул бы `Pending`, и тест повис бы на первом же шаге.
#[test]
fn keeps_ticking_and_never_returns() {
    MockDriver::get().reset();
    let stand = supervisor::stand!(crate::heartbeat::HEARTBEAT_FRAG);
    let mut node = pin!(task(stand.ctx()));
    let mut cx = Context::from_waker(Waker::noop());

    for tick in 0..10 {
        assert!(
            node.as_mut().poll(&mut cx).is_pending(),
            "узел вышел на такте {tick}"
        );
        MockDriver::get().advance(IDLE_PERIOD);
    }
}
