//! Буферы для периферии: zero-copy DMA-буфер.
//!
//! Некомпилируемый (`ignore`) пример — `cross`-workspace не собирается в
//! сыром шаблоне без подстановки `{{chip_feature}}` при генерации.
//!
//! # `bbqueue` — grant/commit API: DMA пишет напрямую в backing storage, без
//! промежуточного копирования CPU. Не дублирует `embassy_sync::Pipe` (тот
//! copy-based, для обычного межзадачного байтового обмена без DMA). Статика
//! (`static UART_TX: BBBuffer<...>`) создаётся только в `cross` — здесь.
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
