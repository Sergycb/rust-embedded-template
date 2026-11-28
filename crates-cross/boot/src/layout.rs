//! Границы разделов обновления, которые нужны bootloader'у, — одно место
//! чтения на всё.
//!
//! Числа — символы `memory.x` этого бинарника, значение символа — его адрес;
//! проверки над ними — `ASSERT` там же (docs/flash.md, «Символы раскладки»).
//! По тем же символам считается отпечаток раскладки ([`fingerprint`]), который
//! сверяет с хвостом образа узел OTA приложения.

#[expect(
    unsafe_code,
    reason = "символы memory.x объявляются только в `unsafe extern`; читается лишь их адрес (`&raw const`): многие из них — смещения, а не адреса памяти, и читать их как значение нельзя"
)]
unsafe extern "C" {
    /// Начало `ACTIVE` — смещение от базы flash; символ `memory.x`, его же
    /// читает `embassy-boot`.
    static __bootloader_active_start: u8;
    /// Конец `ACTIVE` — см. `__bootloader_active_start`.
    static __bootloader_active_end: u8;
    /// Начало `STATE` — для нарезки разделов обмена и отпечатка раскладки,
    /// как и остальные границы `embassy-boot` ниже.
    static __bootloader_state_start: u8;
    /// Конец `STATE`.
    static __bootloader_state_end: u8;
    /// Начало `DFU`.
    static __bootloader_dfu_start: u8;
    /// Конец `DFU`.
    static __bootloader_dfu_end: u8;
{%- if external_flash == "true" %}
    /// Конец последнего раздела на внешней флеши (`memory.x`, `MAX` по концам
    /// разделов): смещение от её начала.
    static __ext_flash_end: u8;
{%- endif %}
}
{%- if external_flash == "true" %}

/// Конец последнего раздела на внешней флеши, байты: меньше микросхема не
/// годится. Его сверяет с объёмом микросхемы `cpu::external::init`.
#[must_use]
pub(crate) fn ext_flash_end() -> u32 {
    value(&raw const __ext_flash_end)
}
{%- endif %}

/// Раздел `ACTIVE`: `(начало, размер)` — смещение от базы flash и длина.
pub(crate) fn active() -> (u32, u32) {
    let start = value(&raw const __bootloader_active_start);
    (start, value(&raw const __bootloader_active_end) - start)
}

/// Раздел `DFU`: `(начало, размер)` — смещение в его носителе и длина. С
/// `DFU` в банке 2 смещение отсчитано от его начала (`memory.x` считает так же).
pub(crate) fn dfu() -> (u32, u32) {
    let start = value(&raw const __bootloader_dfu_start);
    (start, value(&raw const __bootloader_dfu_end) - start)
}

/// Раздел `BOOTLOADER_STATE`: `(начало, размер)` — как у [`dfu`].
pub(crate) fn state() -> (u32, u32) {
    let start = value(&raw const __bootloader_state_start);
    (start, value(&raw const __bootloader_state_end) - start)
}

/// Значение символа линкера — его адрес (адресное пространство МК 32-битное).
fn value(symbol: *const u8) -> u32 {
    symbol as u32
}
