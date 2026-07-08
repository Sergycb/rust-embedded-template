//! Иллюстративный иерархический async-автомат на `statig`.
//!
//! `statig` — runtime HSM с нативной поддержкой async-обработчиков и
//! иерархии состояний; предназначен для использования как компонент внутри
//! произвольной задачи/структуры домена (вызывающий код сам подаёт события
//! через `handle()`), в отличие от `hsmc` в `cross`, где весь чарт — это
//! отдельная задача целиком.
//!
//! `cargo run -p domain --example statig_blinky`

use statig::prelude::*;

pub struct Blinky;

pub enum Event {
    TimerElapsed,
    ButtonPressed,
}

#[state_machine(initial = "State::led_on()")]
impl Blinky {
    #[state]
    async fn led_on(event: &Event) -> Outcome<State> {
        match event {
            Event::TimerElapsed => Transition(State::led_off()),
            Event::ButtonPressed => Super,
        }
    }

    #[state]
    async fn led_off(event: &Event) -> Outcome<State> {
        match event {
            Event::TimerElapsed => Transition(State::led_on()),
            Event::ButtonPressed => Super,
        }
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut state_machine = Blinky.state_machine();

    state_machine.handle(&Event::TimerElapsed).await;
    state_machine.handle(&Event::ButtonPressed).await;
}
