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
//! - `scb_d` — и кэш данных ядра: он стоит перед всей RAM, а DMA его не видит,
//!   поэтому сначала `bsp::dma_nc` помечает в MPU non-cacheable регион под
//!   буферы DMA (у каждого M7, кроме ядра CM4 двухъядерных H7).
{%- if shared_flash == "true" %}
{%- if flash_cache == "true" %}
//!
//! Здесь же — [`Coherent`], обёртка флеша чипа: стирание и запись идут мимо
//! кэша, и после них она его сбрасывает.
{%- endif %}
{%- endif %}

{%- assign flash_cache = "false" %}
{%- if cache contains "scb_d" or cache contains "icache" or cache contains "dcen" %}
{%- assign flash_cache = "true" %}
{%- endif %}

{%- if cache contains "icen" or cache contains "icache" %}
use embassy_stm32::pac;
{%- endif %}
{%- if shared_flash == "true" %}
{%- if flash_cache == "true" %}
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
{%- endif %}
{%- endif %}
{%- if shared_flash == "true" %}
{%- if flash_cache == "true" %}

/// Флеш чипа, после стирания и записи которого кэш не отдаёт старое.
///
/// Ядро читает флеш через кэш ({% if cache contains "scb_d" %}кэш данных ядра Cortex-M7{% elsif cache contains "icache" %}блок `ICACHE`{% else %}кэш данных акселератора флеша, `FLASH_ACR.DCEN`{% endif %}),
/// а стирание и запись идут мимо него, и `embassy-stm32` после них кэш не
/// сбрасывает: прочитанное до стирания так и читалось бы после, и
/// `sequential-storage` с `embassy-boot` видели бы стёртое старым (поймано на
/// NUCLEO-H743 target-тестом `prepare_erases_what_the_swap_moves`). Кэши
/// включает [`enable`] — docs/clocks.md, «Кэши».
pub struct Coherent<F>(F);

// Вручную: у `Flash` из `embassy-stm32` нет `Debug`.
impl<F> core::fmt::Debug for Coherent<F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Coherent").finish_non_exhaustive()
    }
}

impl<F> Coherent<F> {
    /// Оборачивает флеш чипа.
    pub(crate) const fn new(flash: F) -> Self {
        Self(flash)
    }

    /// Сбрасывает кэш, видящий флеш: следующее чтение пойдёт во флеш.
{%- if cache contains "scb_d" %}
    #[expect(unsafe_code, reason = "обоснование — в комментарии ниже")]
{%- endif %}
    fn forget() {
{%- if cache contains "scb_d" %}
        // SAFETY: только обслуживание кэша, под замком флеша, на одном ядре;
        // `SCB` и `CPUID` берутся через `steal()`, как в `enable`.
        let mut core = unsafe { cortex_m::Peripherals::steal() };
        // Весь кэш с выпиской грязных строк, а не по адресам: строки RAM
        // выписываются, а не теряются, и адреса разделов знать не нужно. На
        // кэше в 16 KiB это микросекунды против миллисекунд стирания.
        core.SCB.clean_invalidate_dcache(&mut core.CPUID);
{%- endif %}
{%- if cache contains "icache" %}
        embassy_stm32::pac::ICACHE
            .cr()
            .modify(|w| w.set_cacheinv(true));
        while embassy_stm32::pac::ICACHE.sr().read().busyf() {}
{%- endif %}
{%- if cache contains "dcen" %}
        let acr = embassy_stm32::pac::FLASH.acr();
        // Сброс — только при выключенном кэше данных (RM, `FLASH_ACR.DCRST`).
        if acr.read().dcen() {
            acr.modify(|w| w.set_dcen(false));
            acr.modify(|w| w.set_dcrst(true));
            acr.modify(|w| w.set_dcrst(false));
            acr.modify(|w| w.set_dcen(true));
        }
{%- endif %}
    }
}

impl<F: ErrorType> ErrorType for Coherent<F> {
    type Error = F::Error;
}

impl<F: ReadNorFlash> ReadNorFlash for Coherent<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl<F: NorFlash> NorFlash for Coherent<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let result = self.0.erase(from, to);
        // И при отказе: часть страниц могла успеть стереться.
        Self::forget();
        result
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let result = self.0.write(offset, bytes);
        Self::forget();
        result
    }
}
{%- endif %}
{%- endif %}

/// Включает кэши этого чипа. Зовётся один раз, из `Board::new`, после
/// `embassy_stm32::init` (латентность флеша и тактирование уже выставлены) и до
/// запуска задач.
{%- if cache contains "scb" %}
// `cortex_m::Peripherals::take()` отдаёт периферию ядра один раз за запуск и
// после `embassy_stm32::init` на части семейств уже занят (STM32N6 зовёт
// `steal()` в `rcc/n6.rs`), поэтому `steal()`; флаг `take()` он не трогает.
#[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
{%- endif %}
pub fn enable() {
{%- if cache contains "scb" %}
    // SAFETY: зовётся из `Board::new` раньше остальных задач и прерываний, на одном
    // ядре; `SCB` и `CPUID` здесь только читаются и пишутся регистры кэшей.
    let mut core = unsafe { cortex_m::Peripherals::steal() };
    core.SCB.enable_icache();
{%- if cache contains "scb_d" %}
    // Кэш данных — только после того, как буферы DMA ушли из-под него.
    crate::dma_nc::protect(&mut core.MPU);
    core.SCB.enable_dcache(&mut core.CPUID);
{%- endif %}
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
