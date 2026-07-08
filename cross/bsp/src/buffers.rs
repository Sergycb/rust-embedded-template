//! Буферы и структуры данных для периферии: zero-copy DMA-буфер и
//! intrusive-коллекции без выделения отдельного узла на объект.
//!
//! Некомпилируемый (`ignore`) пример — `cross`-workspace не собирается в
//! сыром шаблоне без подстановки `{{chip_feature}}` при генерации.
//!
//! # `bbqueue` — grant/commit API: DMA пишет напрямую в backing storage, без
//! промежуточного копирования CPU. Не дублирует `embassy_sync::Pipe` (тот
//! copy-based, для обычного межзадачного байтового обмена без DMA).
//! Подключён с `default-features = false, features = ["critical-section"]` —
//! default-фича `maitake-sync-0_3` (async-ожидание грантов) не нужна для
//! синхронного grant/commit ниже и лишь увеличивает размер прошивки.
//!
//! ```ignore
//! use bbqueue::BBBuffer;
//!
//! static UART_TX: BBBuffer<256> = BBBuffer::new();
//!
//! let (mut producer, mut consumer) = UART_TX.try_split().unwrap();
//! let mut grant = producer.grant_exact(64).unwrap();
//! // DMA пишет прямо в grant.buf()
//! grant.commit(64);
//! ```
//!
//! # `intrusive-collections` — объект встраивает link-поле и может состоять
//! сразу в нескольких коллекциях без отдельного выделения узла (кастомные
//! wait-list/scheduler-примитивы). Не дублирует `heapless` (тот копирует
//! значения в коллекцию фиксированной ёмкости).
//!
//! ```ignore
//! use intrusive_collections::{intrusive_adapter, LinkedList, LinkedListLink};
//!
//! struct WaitingTask {
//!     link: LinkedListLink,
//!     id: u32,
//! }
//!
//! intrusive_adapter!(TaskAdapter = &'static WaitingTask: WaitingTask { link: LinkedListLink });
//!
//! let mut wait_list: LinkedList<TaskAdapter> = LinkedList::new(TaskAdapter::new());
//! ```
