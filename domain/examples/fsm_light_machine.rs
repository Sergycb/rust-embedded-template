//! Иерархический автомат как **компонент** внутри произвольной задачи:
//! владелец сам подаёт события через `dispatch()`, автомат ничего не ждёт и
//! ни на чём не крутится — ни исполнителя, ни таймеров, ни async.
//!
//! Пара к `fsm_async_connection_chart.rs`: там тот же самый `fsm::Machine`,
//! но обёрнут в `run()`-цикл и владеет своей задачей целиком. Выбор между
//! ними — про то, кто кого ведёт (владелец автомат или автомат владельца),
//! а не про две разные библиотеки с разными моделями состояний, как было со
//! связкой `statig`/`hsmc` до замены.
//!
//! Иерархия здесь несёт конкретную нагрузку: `Sleep` обрабатывается один раз
//! в суперсостоянии `powered`, а не копируется в каждый лист — событие,
//! которое лист вернул как `Outcome::Ignored`, всплывает к родителю.
//!
//! `cargo run -p domain --example fsm_light_machine`

use fsm::{Outcome, Runtime, state_machine};

struct Sensor;

#[state_machine(id = SensorId, data = SensorData, event = SensorEvent)]
impl Sensor {
    /// Суперсостояние: общий обработчик «выключиться» для всех запитанных
    /// состояний. `default = idle` — куда спуститься, если переход нацелен
    /// на сам композит.
    #[superstate(default = idle)]
    fn powered(_data: &mut SensorData, event: &SensorEvent) -> Outcome<SensorId> {
        match event {
            SensorEvent::Sleep => Outcome::Transition(SensorId::Sleeping),
            _ => Outcome::Ignored,
        }
    }

    #[state(superstate = powered)]
    fn idle(_data: &mut SensorData, event: &SensorEvent) -> Outcome<SensorId> {
        match event {
            SensorEvent::Trigger => Outcome::Transition(SensorId::Measuring),
            _ => Outcome::Ignored,
        }
    }

    #[state(superstate = powered, entry = start_conversion)]
    fn measuring(_data: &mut SensorData, event: &SensorEvent) -> Outcome<SensorId> {
        match event {
            SensorEvent::SampleReady => Outcome::Transition(SensorId::Idle),
            _ => Outcome::Ignored,
        }
    }

    #[state]
    fn sleeping(_data: &mut SensorData, event: &SensorEvent) -> Outcome<SensorId> {
        match event {
            SensorEvent::WakeUp => Outcome::Transition(SensorId::Powered),
            _ => Outcome::Ignored,
        }
    }

    /// Entry-хук `measuring`: в прошивке здесь был бы старт ADC-конверсии.
    fn start_conversion(data: &mut SensorData) {
        data.conversions += 1;
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SensorEvent {
    Trigger,
    SampleReady,
    Sleep,
    WakeUp,
}

struct SensorData {
    conversions: u32,
}

fn main() {
    // Стартуем в композите — конструктор сам спускается по `default(...)`
    // до листа, поэтому текущее состояние сразу `Idle`, а не `Powered`.
    let mut sensor = Runtime::<Sensor>::new(SensorId::Powered, SensorData { conversions: 0 });
    assert_eq!(sensor.state(), SensorId::Idle);

    // Entry-хук взводится на каждом входе в `measuring`, а не один раз.
    sensor.dispatch(&SensorEvent::Trigger);
    assert_eq!(sensor.state(), SensorId::Measuring);
    assert_eq!(sensor.data().conversions, 1);

    sensor.dispatch(&SensorEvent::SampleReady);
    assert_eq!(sensor.state(), SensorId::Idle);

    sensor.dispatch(&SensorEvent::Trigger);
    assert_eq!(sensor.data().conversions, 2);

    // `measuring` не знает про `Sleep` и возвращает `Ignored` — событие
    // всплывает в `powered`, который его и обрабатывает.
    sensor.dispatch(&SensorEvent::Sleep);
    assert_eq!(sensor.state(), SensorId::Sleeping);

    // Переход нацелен на композит `Powered` — снова спуск по `default`.
    sensor.dispatch(&SensorEvent::WakeUp);
    assert_eq!(sensor.state(), SensorId::Idle);
    assert_eq!(sensor.data().conversions, 2);
}
