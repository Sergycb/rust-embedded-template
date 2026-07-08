//! Оркестрация embassy-задач: жизненный цикл, watchdog, компонентная
//! развязка, иерархические стейтчарты.
//!
//! Примеры ниже намеренно не компилируются (`ignore`) — этот `cross`-workspace
//! не может собраться в сыром шаблоне: `embassy-stm32`/`embassy-task-watchdog`
//! завязаны на конкретный `{{chip_feature}}`, подставляемый только при
//! генерации проекта (`cargo generate`). Раскомментируйте и адаптируйте под
//! свою плату по мере необходимости.
//!
//! # `embassy-supervisor` — упорядоченный старт/стоп задач по графу зависимостей
//!
//! ```ignore
//! use embassy_supervisor::supervisor_graph;
//!
//! supervisor_graph! {
//!     usart_task -> app_task,
//! }
//! ```
//!
//! # `embassy-task-watchdog` — мультиплексирование нескольких watchdog'ов задач
//! в один аппаратный watchdog
//!
//! ```ignore
//! use embassy_task_watchdog::WatchdogManager;
//!
//! let manager = WatchdogManager::new(p.IWDG);
//! let handle = manager.register("usart_task");
//! // где-то внутри usart_task: handle.pet().await;
//! ```
//!
//! # `firmware-controller` — компонентная развязка: RPC + pub-sub + периодика
//! в одном макросе (`#[controller]`). Функционально богаче `ector` (тот даёт
//! только fire-and-forget `notify()` между акторами); embassy-sync у него
//! реально no_std-safe — проверено `cargo check --target
//! thumbv7em-none-eabihf`, собирается чисто.
//!
//! ```ignore
//! use firmware_controller::controller;
//!
//! #[controller]
//! impl SensorController {
//!     #[controller(publish)]
//!     fn set_reading(&mut self, value: u16) {}
//!
//!     #[controller(poll_millis = 100)]
//!     async fn sample(&mut self) {}
//! }
//! ```
//!
//! # `hsmc` — иерархический стейтчарт как отдельная задача (таймеры и
//! ISR/cross-task инъекция событий встроены). Используется вместе со
//! `statig` в `domain`, не вместо него: `statig` — компонент внутри
//! произвольной задачи (вызывающий код сам кормит события через `handle()`),
//! `hsmc` — задача, которая целиком является чартом (`m.run().await`).
//!
//! ```ignore
//! hsmc::chart! {
//!     name: ConnectionChart,
//!     initial: Disconnected,
//!     states: {
//!         Disconnected {
//!             on(connect_requested) => Connecting,
//!         },
//!         Connecting {
//!             on(after 5s) => Disconnected,
//!             on(connected) => Connected,
//!         },
//!         Connected {},
//!     }
//! }
//!
//! let mut chart = ConnectionChart::new();
//! let sender = chart.sender(); // клонируемый handle для других задач/ISR
//! chart.run().await; // вся задача целиком
//! ```
