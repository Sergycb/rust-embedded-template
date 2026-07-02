//! Именованные группы ресурсов платы (пины, периферия, DMA-каналы),
//! собранные через [`assign_resources!`](https://docs.rs/assign-resources).
//!
//! Имена полей `embassy_stm32::Peripherals` зависят от выбранного чипа
//! (`{{chip_feature}}`), поэтому шаблон не может зашить готовую распиновку —
//! раскомментируйте пример ниже и замените поля на пины вашей платы.
//!
//! ```ignore
//! use assign_resources::assign_resources;
//! use embassy_stm32::peripherals;
//!
//! assign_resources! {
//!     usart: UsartResources {
//!         tx: PA9,
//!         rx: PA10,
//!         usart: USART1,
//!     }
//! }
//! ```
//!
//! Использование, например в `Board::init`:
//! ```ignore
//! let r = split_resources!(p);
//! spawner.spawn(usart_task(r.usart)).unwrap();
//! ```
