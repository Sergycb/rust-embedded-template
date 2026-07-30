//! Тот же `fsm::Machine`, что в `fsm_light_machine.rs`, но целиком владеет
//! своей задачей: `run()` сам ждёт события из `embassy_sync`-канала и сам
//! взводит таймауты состояний по `embassy_time`. Владельцу остаётся только
//! спавнить эту задачу и отменять её дропом.
//!
//! Чем отличается от синхронного варианта — не набором состояний, а тем, кто
//! ведёт цикл. `timeout`/`on_timeout` в атрибуте состояния объявляют, что
//! `connecting` не может длиться вечно; синтезированное по таймауту событие
//! приходит в тот же `handle()`, что и настоящее, и обрабатывается наравне.
//! Это заменило связку `statig` + `hsmc`, где «компонент» и «задача» были
//! двумя разными библиотеками с несовместимыми декларациями состояний.
//!
//! `run()` не возвращается никогда — это тело задачи, которую снаружи
//! отменяют дропом (в прошивке — `supervisor` из `cross`, здесь — истёкший
//! `tokio::time::timeout`). Отмена cancel-safe: уже поставленное в очередь
//! событие не теряется, недождавшийся таймер просто снимается.
//!
//! `cargo run -p domain --example fsm_async_connection_chart`

use core::time::Duration;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::channel::Channel;
use fsm::{Outcome, state_machine};
use fsm_async::AsyncTimedRuntime;

struct Link;

#[state_machine(id = LinkId, data = LinkData, event = LinkEvent)]
impl Link {
    #[state]
    fn disconnected(_data: &mut LinkData, event: &LinkEvent) -> Outcome<LinkId> {
        match event {
            LinkEvent::Connect => Outcome::Transition(LinkId::Connecting),
            _ => Outcome::Ignored,
        }
    }

    /// Рукопожатие ограничено по времени прямо в декларации состояния:
    /// не пришло `Established` за 50 мс — `run()` синтезирует
    /// `HandshakeTimedOut` и подаёт его как обычное событие.
    #[state(timeout = Duration::from_millis(50), on_timeout = LinkEvent::HandshakeTimedOut)]
    fn connecting(data: &mut LinkData, event: &LinkEvent) -> Outcome<LinkId> {
        match event {
            LinkEvent::Established => Outcome::Transition(LinkId::Connected),
            LinkEvent::HandshakeTimedOut => {
                data.handshake_timeouts += 1;
                Outcome::Transition(LinkId::Disconnected)
            }
            _ => Outcome::Ignored,
        }
    }

    #[state(entry = count_session)]
    fn connected(_data: &mut LinkData, event: &LinkEvent) -> Outcome<LinkId> {
        match event {
            LinkEvent::Disconnect => Outcome::Transition(LinkId::Disconnected),
            _ => Outcome::Ignored,
        }
    }

    fn count_session(data: &mut LinkData) {
        data.sessions += 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum LinkEvent {
    Connect,
    Established,
    Disconnect,
    HandshakeTimedOut,
}

struct LinkData {
    sessions: u32,
    handshake_timeouts: u32,
}

/// Канал событий: в прошивке его отдаёт `supervisor_graph!` через поле
/// `inbox:`, здесь — обычный `static`. `CriticalSectionRawMutex`, а не
/// `NoopRawMutex`: для `static` нужен настоящий `Sync`.
static EVENTS: Channel<CriticalSectionRawMutex, LinkEvent, 4> = Channel::new();

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut link = AsyncTimedRuntime::<Link>::new(
        LinkId::Disconnected,
        LinkData {
            sessions: 0,
            handshake_timeouts: 0,
        },
    );

    tokio::spawn(async {
        // Первая попытка: `Established` не приходит — сработает таймаут
        // состояния `connecting`, автомат сам вернётся в `disconnected`.
        EVENTS.send(LinkEvent::Connect).await;
        tokio::time::sleep(Duration::from_millis(150)).await;
        // Вторая: рукопожатие успевает в окно.
        EVENTS.send(LinkEvent::Connect).await;
        tokio::time::sleep(Duration::from_millis(10)).await;
        EVENTS.send(LinkEvent::Established).await;
        // Закрываем сессию штатно — таймаут `connecting` тут уже ни при чём,
        // у `connected` его нет вовсе (`Timed::timeout` вернёт `None`).
        tokio::time::sleep(Duration::from_millis(10)).await;
        EVENTS.send(LinkEvent::Disconnect).await;
    });

    let outcome =
        tokio::time::timeout(Duration::from_millis(400), link.run(EVENTS.receiver())).await;
    assert!(
        outcome.is_err(),
        "run() не возвращается сам — истечь обязан именно таймаут"
    );

    assert_eq!(link.state(), LinkId::Disconnected);
    // Один разорванный по таймауту хендшейк и одна успешная сессия.
    assert_eq!(link.data().handshake_timeouts, 1);
    assert_eq!(link.data().sessions, 1);
}
