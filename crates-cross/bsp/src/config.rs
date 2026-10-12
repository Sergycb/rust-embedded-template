//! Обвязка раздела настроек этой платы: границы `CONFIG` из символов `memory.x`
//! и псевдоним типа для задач.
//!
//! Само хранилище — `adapters::settings::Settings` (generic по флешу,
//! тестируется на хосте), мост к общему блокирующему `Flash` —
//! `adapters::flash::Shared`. Здесь остаётся то, что знает про чип: раздел —
//! две страницы стирания (меньше `sequential-storage` не берёт), положенные
//! при генерации туда, где они дешевле: с OTA на F2/F4/F7 — в мелкие сектора
//! перед `ACTIVE` (у F4 — 2×16 KiB), иначе в хвост flash. Границы приезжают
//! символами линкера, страница — [`CONFIG_ERASE_SIZE`].

use core::ops::Range;

use adapters::flash::{Paged, Shared};

use crate::board::FlashMutex;

/// Страница раздела `CONFIG` — размер сектора, в котором он лежит.
///
/// `sequential-storage` делит раздел на страницы по `ERASE_SIZE` флеша, а
/// цельный `Flash` объявляет им самый крупный сектор чипа. Раздел в мелких
/// секторах (F2/F4/F7 с OTA) со страницей всего чипа не делится на две, поэтому
/// страница своя (`Paged`); в хвосте flash она совпадает со страницей чипа.
/// Число посчитано при генерации тем же расчётом, что адреса в `memory.x`.
const CONFIG_ERASE_SIZE: usize = {{config_erase_size}};

/// Хранилище настроек этой платы — то, что лежит полем `Board` и уезжает в
/// задачи. Псевдоним по общему правилу шаблона: тип в сигнатуре
/// `#[embassy_executor::task]` должен быть конкретным.
pub type Settings =
    adapters::settings::Settings<Paged<Shared<'static, FlashMutex>, CONFIG_ERASE_SIZE>>;

// Ещё один `#[expect(unsafe_code)]` в `crates-cross` (есть только при `config=yes`):
// причина та же, что у блока в `ota.rs` — адрес линкерного символа иначе не взять.
#[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
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
    adapters::settings::Settings::new(Paged::new(Shared::new(flash)), flash_range())
}

/// Границы раздела по символам линкера.
fn flash_range() -> Range<u32> {
    // SAFETY: символы объявлены линкерным скриптом как абсолютные адреса;
    // читается их адрес, а не содержимое (памяти за ними нет).
    let start = &raw const __config_start as u32;
    let end = &raw const __config_end as u32;
    start..end
}
