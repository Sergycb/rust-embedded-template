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
//! # `ector` — actor-паттерн (message-passing) для embassy-задач: fire-and-
//! forget `notify()` между акторами, каждый актор — отдельная embassy-задача.
//! `firmware-controller` (RPC + pub-sub + периодика в одном макросе)
//! функционально богаче, но сам крейт не объявляет `#![no_std]` ни при какой
//! фиче и физически не собирается на реальном ARM-таргете — проверено
//! `cargo build --target thumbv7em-none-eabihf` с полностью чистым
//! (`rm -rf target`) билдом, падает на E0463 (`std` not found). `ector`
//! реально собирается и работает — проверено на STM32F3Discovery.
//!
//! ```ignore
//! use ector::{actor, Actor, Address, Inbox};
//!
//! struct Counter(u32);
//!
//! impl Actor for Counter {
//!     type Message = u32;
//!
//!     async fn on_mount(&mut self, _: Address<Self>, mut inbox: impl Inbox<Self>) {
//!         loop {
//!             let increment = inbox.next().await;
//!             self.0 += increment;
//!         }
//!     }
//! }
//! ```
//!
//! `hsmc` (иерархический стейтчарт как отдельная задача) сюда не входит —
//! его "embassy"-фича тянет только embassy-sync/time/futures (без
//! embassy-executor), поэтому определение чарта — чистая логика без
//! привязки к железу и живёт в `domain` (см. domain/examples/), а cross
//! только спавнит задачу, вызывающую `chart.run().await`.
