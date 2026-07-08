//! Runtime-конфигурация устройства через `miniconf`: адресуемое дерево
//! настроек поверх вложенных структур, доступ к отдельным "листьям" по
//! пути (JSON/postcard/MQTT), без аллокатора.
//!
//! Некомпилируемый (`ignore`) пример — `cross`-workspace не собирается в
//! сыром шаблоне без подстановки `{{chip_feature}}` при генерации.
//!
//! ```ignore
//! use miniconf::{Leaf, Tree};
//!
//! #[derive(Tree, Default)]
//! struct Settings {
//!     sample_rate_hz: Leaf<u32>,
//!     usart_baud: Leaf<u32>,
//! }
//!
//! let mut settings = Settings::default();
//! settings.set_json("/sample_rate_hz", b"1000")?;
//! ```
