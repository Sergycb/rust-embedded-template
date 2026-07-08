//! Иллюстративный compile-time typestate: недопустимый переход состояния
//! (например, отправка данных при `Disconnected`) — ошибка компиляции, а не
//! runtime-проверка. В отличие от `statig`/`hsmc` (runtime-автоматы, события
//! проверяются в рантайме), `typestate` кодирует состояния как типы —
//! используйте его там, где протокол/последовательность инициализации нужно
//! защитить статически (классика — embedded-hal-паттерн настройки пина).
//!
//! Макрос генерирует только скелет (типы состояний + trait-заготовки с
//! compile-time проверкой графа переходов), тело переходов пишется вручную.
//!
//! `cargo run -p domain --example typestate_connection`

use connection::*;

#[typestate::typestate]
mod connection {
    #[automaton]
    pub struct Connection;

    #[state]
    pub struct Disconnected;
    pub trait Disconnected {
        fn open() -> Disconnected;
        fn connect(self) -> Connected;
    }

    #[state]
    pub struct Connected;
    pub trait Connected {
        fn disconnect(self) -> Disconnected;
        fn close(self);
    }
}

impl DisconnectedState for Connection<Disconnected> {
    fn open() -> Connection<Disconnected> {
        Self {
            state: Disconnected,
        }
    }

    fn connect(self) -> Connection<Connected> {
        Connection::<Connected> { state: Connected }
    }
}

impl ConnectedState for Connection<Connected> {
    fn disconnect(self) -> Connection<Disconnected> {
        Connection::<Disconnected> {
            state: Disconnected,
        }
    }

    fn close(self) {}
}

fn main() {
    let link = Connection::<Disconnected>::open();
    let link = link.connect();
    let link = link.disconnect();
    let link = link.connect();
    link.close();
}
