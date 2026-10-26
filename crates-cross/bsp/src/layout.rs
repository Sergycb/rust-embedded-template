//! Границы разделов и регионов памяти, которые нужны рычагам платы
//! (`bsp::flash`{% if external_flash == "true" %}, `bsp::external`{% endif %}{% if dma_nc == "true" %}, `bsp::dma_nc`{% endif %}), — одно место чтения на все.
//!
//! Числа — символы `memory.x`: линкер вычисляет их из регионов раскладки, и
//! значение символа — его адрес. Задаёт их генерация (`chip-select.rhai`)
//! в `memory.x` каждого бинарника, который линкует `bsp`; там же — `ASSERT`'ы,
//! которые раньше были `const`-assert'ами здесь (выравнивание `CONFIG`,
//! пригодность регионов DMA для MPU): нарушение роняет линковку, до прошивки.
//! Числами из `[env]` эти границы не ездят — раскладка живёт в одном месте, и
//! ручная правка `memory.x` не разойдётся с кодом.

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
{%- endif %}
{%- if config_store == "flash" or config_store == "external" %}
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
{%- if dma_nc == "true" %}
    /// Начало региона `DMA_NC` (`memory.x`, `ORIGIN(DMA_NC)`).
    static __dma_nc_start: u8;
    /// Конец региона `DMA_NC` (`ORIGIN + LENGTH`).
    static __dma_nc_end: u8;
{%- endif %}
{%- if bdma_nc == "true" %}
    /// Начало `SRAM4` — памяти BDMA (`memory.x`, `ORIGIN(SRAM4)`).
    static __bdma_nc_start: u8;
    /// Конец `SRAM4` (`ORIGIN + LENGTH`).
    static __bdma_nc_end: u8;
{%- endif %}
}
{%- if external_flash == "true" %}

/// Конец последнего раздела на внешней флеши, байты: меньше микросхема не
/// годится. Его сверяют с объёмом микросхемы `bsp::external` и `boot`.
#[must_use]
pub fn ext_flash_end() -> u32 {
    value(&raw const __ext_flash_end)
}
{%- endif %}
{%- if ota == "true" %}

/// Размер `ACTIVE` в байтах: самый длинный образ, который доедет до
/// устройства. Это `ACTIVE`, а НЕ `DFU`, хотя принимается образ в `DFU`.
/// Раздел обновления по построению больше активного на одну-две страницы (этого
/// требует `assert_partitions` в `embassy-boot`: обмену нужна запасная
/// страница), а обмен копирует ровно `ACTIVE`. Образ размером между ними прошёл
/// бы и проверку длины, и подпись, и пометку к обмену — а в `ACTIVE` приехал бы
/// обрезанным: устройство отработало бы полный цикл обновления с перезагрузкой и
/// молча откатилось.
pub(crate) fn active_len() -> u32 {
    value(&raw const __bootloader_active_end) - value(&raw const __bootloader_active_start)
}
{%- endif %}
{%- if config_store == "flash" or config_store == "external" %}

/// Раздел настроек: `(начало, размер)` — смещение {% if config_store == "flash" %}от базы flash{% else %}от начала внешней флеши{% endif %} и длина.
/// Границы кратны его странице — `ASSERT` в `memory.x`{% if config_store == "flash" %}: иначе `Partition::new`
/// паникует на старте{% endif %}.
pub(crate) fn config() -> (u32, u32) {
    let start = value(&raw const __config_start);
    (start, value(&raw const __config_end) - start)
}
{%- endif %}
{%- if dma_nc == "true" %}

/// Регион `DMA_NC`: `(адрес, размер)`. Годность для MPU (размер — степень двойки
/// от 32 байт, адрес кратен размеру) держит `ASSERT` в `memory.x`.
pub(crate) fn dma_nc() -> (u32, u32) {
    let start = value(&raw const __dma_nc_start);
    (start, value(&raw const __dma_nc_end) - start)
}
{%- endif %}
{%- if bdma_nc == "true" %}

/// `SRAM4` целиком — память BDMA: `(адрес, размер)`; `ASSERT` — как у `dma_nc`.
pub(crate) fn bdma_nc() -> (u32, u32) {
    let start = value(&raw const __bdma_nc_start);
    (start, value(&raw const __bdma_nc_end) - start)
}
{%- endif %}

/// Значение символа линкера — его адрес (адресное пространство МК 32-битное).
fn value(symbol: *const u8) -> u32 {
    symbol as u32
}
