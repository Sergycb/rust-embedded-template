//! Узел `HEARTBEAT` графа задач: каркас под работу вашего устройства.
//!
//! Задача живёт здесь, а не рядом с графом в `crates-cross/app`, по общему
//! правилу проекта: граф — это оркестрация (кто кого спавнит, что делать с
//! упавшим узлом, чем кормить сторож), а то, что узел делает, — логика.
//! Логика тестируется на хосте и про чип не знает.
//!
//! Задача — автомат `#[fsm::typestate]` на контексте узла. Контекст
//! `HeartbeatCtx` эмитит фрагмент ниже (`ctx: $crate::heartbeat::HeartbeatCtx`)
//! здесь же, в `domain`, а не compose-site: поэтому сигнатура не называет ни
//! одного типа из крейта, где объявлен граф, и автомат с его действиями живёт
//! рядом с узлом. Так же устроен и пример в самом `supervisor`
//! (`supervisor-logic-fixture`).

mod timing;

#[cfg(test)]
mod tests {
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
}

pub use timing::WATCHDOG;

use defmt_or_log::info;
use embassy_time::Instant;

use timing::IDLE_PERIOD;

// Узел графа объявлен здесь же, где лежит его задача: фрагмент — законченный
// модуль с одной задачей, её политикой и таймаутом, а граф в
// `crates-cross/app/src/graph.rs` только перечисляет фрагменты и добавляет
// то, что знает про железо (`boot:`, блок `watchdog:`).
//
// `supervisor_fragment!` объявляет СРЕЗ графа: раскрывается он в один
// `#[macro_export] macro_rules!`-ретранслятор и больше ни во что — ни задач,
// ни статиков, ни `spawn_all`. Отсюда три следствия:
//
// * Ретранслятор виден по пути от корня крейта — `::domain::HEARTBEAT_FRAG`,
//   как бы глубоко ни лежал этот модуль. Имя `HEARTBEAT_FRAG` compose-site и
//   называет в своём `fragments: [..]`.
// * Узел не носит имени — его даёт запись `fragments:`
//   (`HEARTBEAT_FRAG as HEARTBEAT`), и тот же фрагмент, названный дважды под
//   разными `as`, — два узла. Поэтому ниже `node …` без имени, а «узел
//   HEARTBEAT» в этом файле — про то, как его зовёт граф шаблона.
// * Токены ниже резолвятся НЕ здесь, а на compose-site: так работает
//   `macro_rules!`. Поэтому всё, что фрагмент называет, написано полным путём
//   — `$crate::heartbeat::WATCHDOG`, `$crate::heartbeat::task` — и от
//   compose-site ничего не требует: голое имя (`WATCHDOG`) искалось бы в
//   `graph.rs` и падало бы там с `E0425`. `$crate::` при этом указывает сюда
//   и переживает переименование зависимости в чужом манифесте
//   (`logic = { package = "domain" }`), а `crate::` в теле `macro_rules!`
//   означал бы крейт, который фрагмент ВЫЗЫВАЕТ.
//
// Единственное, что фрагмент требует от графа, — блок `watchdog:`: узел с
// полем `watchdog:` без него не компилируется, а сам блок — решение на весь
// граф (один аппаратный сторож, один `check_every`), и фрагменту его
// объявлять нельзя.
supervisor::supervisor_fragment! {
    name: HEARTBEAT_FRAG;

    node watchdog: $crate::heartbeat::WATCHDOG observe,
        ctx: $crate::heartbeat::HeartbeatCtx,
        task: $crate::heartbeat::task;
}

#[fsm::typestate]
impl HeartbeatCtx<'_> {
    fsm::machine! {
        machine Heartbeat mod edges {
            states {
                /// Холостой ход: ждёт такт в `next`.
                Idle { next: Instant },
            }
            start => begin -> Idle;
            listen {
                Idle { next } => tick: fsm::deadline(*next),
            }
            transitions {
                Idle + tick(()) => idle -> self,
            }
        }
    }

    /// Сообщает о старте и назначает первый такт.
    #[expect(
        clippy::unused_self,
        reason = "сигнатуру действия задаёт таблица `#[fsm::typestate]`"
    )]
    fn begin(&mut self) -> Idle {
        info!("domain: узел HEARTBEAT запущен");
        Idle {
            next: Instant::now() + IDLE_PERIOD,
        }
    }

    /// Такт холостого хода: здесь ваша работа, отметка у сторожа — после неё.
    ///
    /// Следующий такт — `fsm::next_tick`, а не `Ticker`: отставший `Ticker`
    /// догоняет пачкой тиков подряд, и работа выполнилась бы N раз разом;
    /// `next_tick` пропущенные такты отбрасывает.
    fn idle(&mut self, s: &mut Idle) {
        self.heartbeat.feed();
        s.next = fsm::next_tick(s.next, IDLE_PERIOD, Instant::now());
    }
}

/// Узел графа — каркас под вашу работу.
///
/// Здесь он только сообщает о старте и отмечается у сторожа в холостом цикле.
/// Это осознанно: шаблон не знает, что должно делать ваше устройство, а
/// периодический лог в пустом узле — это шум, вытесняющий из буфера RTT
/// строки, которые важны. Поэтому цикл молчит.
///
/// **`feed()` зовётся явно и только после реального прогресса** — здесь
/// прогресса нет, поэтому отметка стоит в действии такта. Когда появится
/// работа, отметку ставьте ПОСЛЕ неё, а не до: автоматического «задача жива,
/// раз её future опрашивают» тут нет намеренно, иначе сторож сторожил бы
/// исполнитель, а не полезную работу.
///
/// У автомата нет выходного состояния, `task` не возвращается никогда: узлы
/// шаблона не падают и не перезапускаются (у них нет `restart:`), а на отказ
/// связи пишут в лог и продолжают.
///
/// Что дальше: добавьте автомату состояния и строки переходов, узлу —
/// `deps:` (порядок старта), `inbox:` (очередь событий — источник в
/// `listen`), `resources:` (ручка периферии, переживающая перезапуск задачи)
/// — всё это описано в `docs/modules/app-graph.md`.
pub async fn task(mut ctx: HeartbeatCtx<'_>) {
    match ctx.run().await {}
}
