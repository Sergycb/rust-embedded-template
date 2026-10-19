//! Кэши чипа: включаются один раз на старте, в `Board::new` сразу за
//! `embassy_stm32::init`.
//!
//! С завода они выключены, а HAL их не трогает (кроме `flash`, который лишь
//! гасит и возвращает кэш данных вокруг стирания). Какие кэши есть у этого
//! чипа, известно при генерации (метаданные `stm32-metapac`), поэтому в
//! [`enable`] остаются только они:
//!
//! - `icen`/`dcen` — кэши инструкций и данных акселератора флеша (`FLASH_ACR`:
//!   F2/F4, G0/G4, L4, C0, U0, WB, WL); кэш данных видит только чтение флеша;
//! - `icache` — блок `ICACHE` (L5, U3/U5, H5, WBA);
//! - `scb` — кэш инструкций ядра Cortex-M7 (F7, H7, H7R/S);
//! - `scb_d` — и кэш данных ядра, только у чипов с `AXISRAM` (H7): он стоит
//!   перед всей RAM, а DMA его не видит, поэтому сначала `bsp::dma_nc` выделяет
//!   хвост `AXISRAM` под буферы DMA и помечает его в MPU non-cacheable.
//!   У F7 и H7R/S такого блока нет, и кэш данных им не включается.

{%- if cache contains "icen" or cache contains "icache" %}
use embassy_stm32::pac;
{%- endif %}

/// Включает кэши этого чипа. Зовётся один раз, из `Board::new`, после
/// `embassy_stm32::init` (латентность флеша и тактирование уже выставлены) и до
/// запуска задач.
pub fn enable() {
{%- if cache contains "scb" %}
    enable_core_caches();
{%- endif %}
{%- if cache contains "icen" %}
    pac::FLASH.acr().modify(|w| {
        w.set_icen(true);
{%- if cache contains "dcen" %}
        w.set_dcen(true);
{%- endif %}
    });
{%- endif %}
{%- if cache contains "icache" %}
    pac::ICACHE.cr().modify(|w| w.set_en(true));
{%- endif %}
}
{%- if cache contains "scb" %}

// `cortex_m::Peripherals::take()` отдаёт периферию ядра один раз за запуск и
// после `embassy_stm32::init` на части семейств уже занят (STM32N6 зовёт
// `steal()` в `rcc/n6.rs`), поэтому `steal()`; флаг `take()` он не трогает.
#[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
fn enable_core_caches() {
    // SAFETY: зовётся из `Board::new` до запуска задач и прерываний, на одном
    // ядре; `SCB` и `CPUID` здесь только читаются и пишутся регистры кэшей.
    let mut core = unsafe { cortex_m::Peripherals::steal() };
    core.SCB.enable_icache();
{%- if cache contains "scb_d" %}
    // Кэш данных — только после того, как буферы DMA ушли из-под него.
    crate::dma_nc::protect(&mut core.MPU);
    core.SCB.enable_dcache(&mut core.CPUID);
{%- endif %}
}
{%- endif %}
