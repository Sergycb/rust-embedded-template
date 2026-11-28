//! Границы разделов, которые нужны приложению (`crate::storage`), — одно
//! место чтения на все.
//!
//! Числа — символы `memory.x` этого бинарника, значение символа — его адрес;
//! проверки над ними — `ASSERT` там же (docs/flash.md, «Символы раскладки»).
//!{% if ota == "true" %} По тем же символам прошивка считает и отпечаток раскладки
//! ([`fingerprint`]), который сверяет с хвостом образа узел OTA.{% endif %}
{%- assign need_config = "false" %}
{%- if config_store == "flash" or config_store == "external" %}
{%- assign need_config = "true" %}
{%- endif %}
{%- if ota == "true" or need_config == "true" or external_flash == "true" %}

#[expect(
    unsafe_code,
    reason = "символы memory.x объявляются только в `unsafe extern`; читается лишь их адрес (`&raw const`): многие из них — смещения, а не адреса памяти, и читать их как значение нельзя"
)]
unsafe extern "C" {
{%- if ota == "true" %}
    /// Начало `ACTIVE` — смещение от базы flash; символ `memory.x`, его же
    /// читает `embassy-boot`.
    static __bootloader_active_start: u8;
    /// Конец `ACTIVE` — см. `__bootloader_active_start`.
    static __bootloader_active_end: u8;
    /// Начало `STATE` — для нарезки разделов обновления и отпечатка раскладки,
    /// как и остальные границы `embassy-boot` ниже.
    static __bootloader_state_start: u8;
    /// Конец `STATE`.
    static __bootloader_state_end: u8;
    /// Начало `DFU`.
    static __bootloader_dfu_start: u8;
    /// Конец `DFU`.
    static __bootloader_dfu_end: u8;
{%- endif %}
{%- if need_config == "true" %}
    /// Начало раздела настроек (`memory.x`): смещение {% if config_store == "flash" %}от базы flash — так
    /// его ждёт `embassy_stm32::flash::Flash`{% else %}от начала внешней флеши{% endif %}.
    static __config_start: u8;
    /// Конец раздела настроек — см. `__config_start`.
    static __config_end: u8;
{%- endif %}
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
pub fn ext_flash_end() -> u32 {
    value(&raw const __ext_flash_end)
}
{%- endif %}
{%- if ota == "true" %}

/// Размер `ACTIVE` в байтах: самый длинный образ, который доедет до
/// устройства. Это `ACTIVE`, а НЕ `DFU`, хотя принимается образ в `DFU`
/// (docs/ota.md, «Приём образа: стирание отдельно от записи»).
pub(crate) fn active_len() -> u32 {
    value(&raw const __bootloader_active_end) - value(&raw const __bootloader_active_start)
}

/// Отпечаток раскладки этой прошивки ([`ports::layout_fingerprint`]) — по
/// символам `memory.x` в порядке [`ports::LAYOUT_SYMBOLS`]. Тот же отпечаток
/// `cargo xtask build` считает по значениям символов в ELF и кладёт в хвост
/// образа; узел OTA сверяет их перед пометкой образа к обмену.
pub(crate) fn fingerprint() -> u32 {
    ports::layout_fingerprint([
        value(&raw const __bootloader_state_start),
        value(&raw const __bootloader_state_end),
        value(&raw const __bootloader_active_start),
        value(&raw const __bootloader_active_end),
        value(&raw const __bootloader_dfu_start),
        value(&raw const __bootloader_dfu_end),
    ])
}

/// Раздел `DFU`: `(начало, размер)` — смещение в его носителе и длина. Те же
/// символы читает `embassy-boot` в bootloader'е, поэтому разделы приложения и
/// bootloader'а совпадают по построению.
pub(crate) fn dfu() -> (u32, u32) {
    let start = value(&raw const __bootloader_dfu_start);
    (start, value(&raw const __bootloader_dfu_end) - start)
}

/// Раздел `BOOTLOADER_STATE`: `(начало, размер)` — как у [`dfu`].
pub(crate) fn state() -> (u32, u32) {
    let start = value(&raw const __bootloader_state_start);
    (start, value(&raw const __bootloader_state_end) - start)
}
{%- endif %}
{%- if need_config == "true" %}

/// Раздел настроек: `(начало, размер)` — смещение {% if config_store == "flash" %}от базы flash{% else %}от начала внешней флеши{% endif %} и длина.
/// Границы кратны его странице — `ASSERT` в `memory.x`{% if config_store == "flash" %}: иначе `Partition::new`
/// паникует на старте{% endif %}.
pub(crate) fn config() -> (u32, u32) {
    let start = value(&raw const __config_start);
    (start, value(&raw const __config_end) - start)
}
{%- endif %}

/// Значение символа линкера — его адрес (адресное пространство МК 32-битное).
fn value(symbol: *const u8) -> u32 {
    symbol as u32
}
{%- endif %}
