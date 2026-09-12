//! Обвязка раздела настроек этой платы: границы `CONFIG` из символов `memory.x`
//! и псевдоним типа для задач.
//!
//! Само хранилище — `adapters::settings::Settings` (generic по флешу,
//! тестируется на хосте), мост к общему блокирующему `Flash` —
//! `adapters::flash::Shared`. Здесь остаётся то, что знает про чип: раздел
//! отрезан от хвоста flash при генерации (ровно две последние страницы
//! стирания — меньше `sequential-storage` не берёт; страница здесь — это
//! `NorFlash::ERASE_SIZE` цельного `Flash`, то есть МАКСИМАЛЬНЫЙ сектор чипа,
//! и на F4/F7/H7 раздел стоит 256 KiB), а его границы приезжают символами
//! линкера.

use core::ops::Range;

use embassy_stm32::flash::{Blocking, Flash};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;

use crate::FlashMutex;

/// Хранилище настроек этой платы — то, что лежит полем `Board` и уезжает в
/// задачи. Псевдоним по общему правилу шаблона: тип в сигнатуре
/// `#[embassy_executor::task]` должен быть конкретным.
pub type Settings = adapters::settings::Settings<
    adapters::flash::Shared<'static, NoopRawMutex, Flash<'static, Blocking>>,
>;

unsafe extern "C" {
    /// Границы раздела `CONFIG` из `memory.x`, отсчитанные от базы flash —
    /// именно так их ждёт `embassy_stm32::flash::Flash` (у него нулевое
    /// смещение это база, а не адрес в адресном пространстве).
    static __config_start: u32;
    static __config_end: u32;
}

/// Собирает хранилище над разделом `CONFIG`.
///
/// # Паника
///
/// Если раздел не годится под `sequential-storage` — см.
/// `adapters::settings::Settings::new`.
pub(crate) fn new(flash: &'static FlashMutex) -> Settings {
    adapters::settings::Settings::new(adapters::flash::Shared::new(flash), flash_range())
}

/// Границы раздела по символам линкера.
fn flash_range() -> Range<u32> {
    // SAFETY: символы объявлены линкерным скриптом как абсолютные адреса;
    // читается их адрес, а не содержимое (памяти за ними нет).
    let start = &raw const __config_start as u32;
    let end = &raw const __config_end as u32;
    start..end
}
