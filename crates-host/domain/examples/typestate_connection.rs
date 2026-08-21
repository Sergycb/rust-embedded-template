//! Compile-time автомат: недопустимый переход (отправка данных при
//! `Disconnected`) — ошибка компиляции, а не runtime-проверка. В отличие от
//! `fsm`/`fsm-async` (runtime-автоматы: события приходят из канала и
//! проверяются в рантайме), здесь состояние живёт в системе типов —
//! применяйте там, где протокол или последовательность инициализации нужно
//! защитить статически (классика — настройка пина в embedded-hal).
//!
//! Заменил одноимённый крейт `typestate` 0.9.0-rc2. Тот не объявлял
//! `#![no_std]` ни при какой конфигурации фич и падал с E0463 на
//! `thumbv7em-none-eabihf` — годился только в `[dev-dependencies]`, то есть
//! защитить типами реальный драйвер в прошивке им было нельзя. Этот
//! `no_std` по-настоящему и объявлен обычной зависимостью.
//!
//! Второе отличие, видное ниже: поля per-state. Данные, осмысленные только
//! в одном состоянии (`session_id` у `Connected`), лежат в самом состоянии,
//! а не как `Option<T>` на общей структуре, который пришлось бы вручную
//! сбрасывать в каждом переходе и `unwrap()`-ать при чтении. Тела переходов
//! пишутся руками — макрос генерирует только типы состояний и trait-каркас
//! с проверкой графа переходов.
//!
//! `cargo run -p domain --example typestate_connection`

use typestate::typestate;

#[typestate]
mod link {
    /// Данные, не зависящие от состояния: живут на автомате целиком.
    #[automaton]
    pub struct Link {
        pub attempts: u32,
    }

    #[state]
    pub struct Disconnected;

    /// Данные, осмысленные только в этом состоянии.
    #[state]
    pub struct Connected {
        pub session_id: u32,
    }

    pub trait Disconnected {
        fn open() -> Disconnected;
        fn connect(self, session_id: u32) -> Connected;
        fn shutdown(self);
    }

    pub trait Connected {
        fn disconnect(self) -> Disconnected;
    }

    impl DisconnectedState for Link<Disconnected> {
        fn open() -> Link<Disconnected> {
            Link {
                attempts: 0,
                state: Disconnected,
            }
        }

        fn connect(self, session_id: u32) -> Link<Connected> {
            Link {
                attempts: self.attempts + 1,
                state: Connected { session_id },
            }
        }

        fn shutdown(self) {}
    }

    impl ConnectedState for Link<Connected> {
        fn disconnect(self) -> Link<Disconnected> {
            Link {
                attempts: self.attempts,
                state: Disconnected,
            }
        }
    }

    /// Поле `state` приватно — читателю снаружи модуля нужен вот такой
    /// обычный inherent-`impl`, написанный внутри него.
    impl Link<Connected> {
        pub fn session_id(&self) -> u32 {
            self.state.session_id
        }
    }
}

use link::{ConnectedState, Disconnected, DisconnectedState, Link};

fn main() {
    let link = Link::<Disconnected>::open();

    // link.session_id() здесь не скомпилируется: метод существует только
    // для Link<Connected>, а не для Link<Disconnected>.
    let link = link.connect(7);
    assert_eq!(link.session_id(), 7);
    assert_eq!(link.attempts, 1);

    let link = link.disconnect();
    let link = link.connect(8);
    assert_eq!(link.session_id(), 8);
    assert_eq!(link.attempts, 2);

    link.disconnect().shutdown();
}
