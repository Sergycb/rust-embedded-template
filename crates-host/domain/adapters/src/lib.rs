#![cfg_attr(not(test), no_std)]

//! Адаптеры — реализации портов из `ports`, которым не нужен конкретный чип.
//!
//! Правило деления с `crates-cross/bsp`: пока реализации хватает трейтов
//! `embedded-hal`/`embedded-storage` (шина или раздел флеша передаётся
//! параметром) — она лежит здесь и тестируется на хосте на фейке; как только
//! понадобится `embassy_stm32::peripherals`, линкерный символ или статический
//! DMA-буфер — это уже железо, и место ему в `bsp` (docs/architecture.md).
//! `bsp` при этом остаётся обвязкой: строит разделы из символов `memory.x`,
//! отдаёт им псевдонимы (задачи embassy не generic) и кладёт в `Board`.

/// Обновление прошивки поверх `embassy-boot`: разделы `DFU`/`BOOTLOADER_STATE`
/// как два `NorFlash`.
pub mod ota;

#[cfg(test)]
pub(crate) mod mem_flash;
