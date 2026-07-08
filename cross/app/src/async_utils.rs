//! Async-примитивы сверх `embassy-futures`/`embassy-sync`.
//!
//! Как и в `task_orchestration`, примеры ниже некомпилируемые (`ignore`) —
//! `cross`-workspace не собирается в сыром шаблоне без подстановки
//! `{{chip_feature}}` при генерации.
//!
//! # `futures-concurrency` — `race!`/`merge` для потоков, произвольное число
//! веток (сверх `embassy_futures::select`/`select3`/`select4`)
//!
//! ```ignore
//! use futures_concurrency::future::Race;
//!
//! let winner = (read_button(), read_timeout()).race().await;
//! ```
//!
//! # `aselect` — как `embassy_futures::select`, но непроигравшие ветки
//! гарантированно не отменяются на середине (cancellation-safety)
//!
//! ```ignore
//! use aselect::select;
//!
//! select! {
//!     res = channel.send(msg) => handle_sent(res),
//!     _ = ticker.next() => {}
//! }
//! ```
//!
//! # `wg` — WaitGroup: дождаться завершения N задач (в `embassy-sync` нет
//! готового аналога)
//!
//! ```ignore
//! use wg::AsyncWaitGroup;
//!
//! let wg = AsyncWaitGroup::new();
//! for _ in 0..3 {
//!     let wg = wg.add(1);
//!     spawner.spawn(worker(wg)).ok();
//! }
//! wg.wait().await;
//! ```
//!
//! # `sync_wrapper` — сделать `!Sync`-тип `Sync` там, где эксклюзивный
//! доступ гарантирован вручную (multi-executor/multi-core сценарии)
//!
//! ```ignore
//! use sync_wrapper::SyncWrapper;
//!
//! struct Shared {
//!     cell: SyncWrapper<core::cell::RefCell<u32>>,
//! }
//! ```
//!
//! # `embedded-rpc` — межзадачный (не host<->target!) request/response с
//! zero-copy буферами (`&mut [u8]` пишется сервером прямо в память клиента)
//!
//! ```ignore
//! use embassy_time::{with_timeout, Duration};
//! use embedded_rpc::RpcService;
//!
//! static SERVICE: RpcService<Request, Response> = RpcService::new();
//!
//! // сервер:
//! let served = SERVICE.serve().await;
//! served.respond(handle(served.request())).await;
//!
//! // клиент, с внешним таймаутом (в крейте таймаута нет из коробки):
//! let response = with_timeout(Duration::from_millis(50), SERVICE.request(req)).await;
//! ```
