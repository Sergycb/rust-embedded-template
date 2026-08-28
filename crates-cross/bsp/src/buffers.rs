//! Буферы для периферии: zero-copy DMA-буфер.
//!
//! Некомпилируемый (`ignore`) пример — `cross`-workspace не собирается в
//! сыром шаблоне без подстановки `{{chip_feature}}` при генерации.
//!
//! # `bbqueue` — grant/commit API: DMA пишет напрямую в backing storage, без
//! промежуточного копирования CPU. Не дублирует `embassy_sync::Pipe` (тот
//! copy-based, для обычного межзадачного байтового обмена без DMA). Статика
//! создаётся только в `cross` — здесь. Подключён с
//! `default-features = false, features = ["critical-section"]` — default-фича
//! `maitake-sync-0_3` (async-ожидание грантов) не нужна для синхронного
//! grant/commit ниже и лишь увеличивает размер прошивки.
//!
//! Пример под API 0.7, тот, что в проекте. Здесь стоял код от 0.5 (`BBBuffer`
//! и `try_split()`) — типов с такими именами в крейте больше нет вовсе, и
//! заметить это было нечем: блок `ignore`, компилятор его не трогает. Меняете
//! версию `bbqueue` — перечитайте и это.
//!
//! `Inline<N>` — буфер прямо в статике (без аллокатора), `AtomicCoord` —
//! курсоры на атомиках, `Polling` — без async-уведомлений, ровно под
//! синхронный цикл ниже.
//!
//! ```ignore
//! use bbqueue::{
//!     queue::BBQueue,
//!     traits::{coordination::cas::AtomicCoord, notifier::polling::Polling, storage::Inline},
//! };
//!
//! static UART_TX: BBQueue<Inline<256>, AtomicCoord, Polling> = BBQueue::new();
//!
//! let producer = UART_TX.stream_producer();
//! let consumer = UART_TX.stream_consumer();
//!
//! let mut grant = producer.grant_exact(64).unwrap();
//! // DMA пишет прямо в grant — это `&mut [u8]`.
//! grant.commit(64);
//!
//! let read = consumer.read().unwrap();
//! let sent = read.len();
//! read.release(sent);
//! ```
