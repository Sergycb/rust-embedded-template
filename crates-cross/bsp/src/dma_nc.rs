//! Память для буферов DMA при включённом кэше данных ядра Cortex-M7.
//!
//! DMA не видит кэш данных ядра: буфер, в который пишет периферия (Ethernet,
//! SDMMC, DMA-приёмник), читается из кэша устаревшим, а буфер, из которого она
//! читает, может не успеть выйти из кэша в RAM. Драйверы `embassy-stm32` кэш не
//! обслуживают. Поэтому в `memory.x` выделен регион `DMA_NC`, а [`protect`]
//! помечает его в MPU как normal non-cacheable; у H7 так же размечается и
//! `SRAM4` целиком — единственная память BDMA (SPI6, I2C4, LPUART1, ADC3).
//! Кэш данных включается только после этого (`bsp::cache`).
//!
//! Буфер DMA кладётся в регион атрибутом секции:
//!
//! ```ignore
//! #[unsafe(link_section = ".dma_nc")] // BDMA — ".bdma_nc"
//! static mut RX: MaybeUninit<[u8; 2048]> = MaybeUninit::uninit();
//! ```
//!
//! Секции `NOLOAD`: `cortex-m-rt` их не инициализирует, начальное содержимое
//! не определено — заводите буфер как `MaybeUninit` и пишите в него явно (для
//! `static_cell::StaticCell` это значит `StaticCell` вне региона: его флаг
//! занятости без нуля не работает). Переполнение региона ловит линкер.
//!
//! Границы регионов — символы `memory.x` (`bsp::layout`), из регионов `DMA_NC`
//! и `SRAM4`; требования MPU к ним (размер — степень двойки не меньше 32 байт,
//! адрес кратен размеру) держат `ASSERT`'ы там же — на линковке, до прошивки.

use cortex_m::peripheral::MPU;

/// Сколько регионов размечается: `DMA_NC`{% if bdma_nc == "true" %} и `SRAM4`{% endif %}.
const REGIONS: usize = {% if bdma_nc == "true" %}2{% else %}1{% endif %};

/// Номер региона MPU первого региона: старшие номера остаются проекту
/// (выше номер — выше приоритет при перекрытии, но перекрытий здесь нет).
const FIRST_REGION: u32 = 7;
const CTRL_ENABLE: u32 = 1 << 0;
/// Привилегированному коду без регионов остаётся карта по умолчанию.
const CTRL_PRIVDEFENA: u32 = 1 << 2;
const RBAR_VALID: u32 = 1 << 4;

// Номера идут от `FIRST_REGION` вниз до нуля: регионов не больше восьми.
const _: () = assert!(REGIONS <= FIRST_REGION as usize + 1);

/// Помечает регионы буферов DMA в MPU non-cacheable и включает MPU. Зовётся один
/// раз, из `bsp::cache`, до включения кэша данных.
///
/// # Panics
///
/// Если у ядра нет MPU или в нём меньше восьми регионов: кэш данных остался бы
/// без защиты буферов DMA.
#[expect(
    unsafe_code,
    reason = "регистры MPU — по указателю ядра, обёрток без unsafe нет"
)]
pub fn protect(mpu: &mut MPU) {
    let available = (mpu._type.read() >> 8) & 0xFF;
    assert!(
        available > FIRST_REGION,
        "MPU: регионов меньше {}, буферы DMA нечем защитить",
        FIRST_REGION + 1
    );
    // SAFETY: зовётся из `Board::new` до запуска задач и прерываний; MPU на
    // время настройки выключен, затем включается с картой по умолчанию.
    unsafe {
        mpu.ctrl.write(0);
        cortex_m::asm::dmb();
        let mut region = FIRST_REGION;
        for (start, len) in regions() {
            mpu.rbar.write(start | RBAR_VALID | region);
            mpu.rasr.write(rasr(len));
            region -= 1;
        }
        mpu.ctrl.write(CTRL_ENABLE | CTRL_PRIVDEFENA);
    }
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

/// Регионы MPU, по порядку: `(адрес, размер)`; первому достаётся [`FIRST_REGION`],
/// каждому следующему — на единицу меньше. `DMA_NC` — буферы DMA1/DMA2, MDMA,
/// Ethernet, SDMMC{% if bdma_nc == "true" %}; `SRAM4` целиком — буферы BDMA{% endif %}.
fn regions() -> [(u32, u32); REGIONS] {
    [crate::layout::dma_nc(){% if bdma_nc == "true" %}, crate::layout::bdma_nc(){% endif %}]
}

/// `RASR`: XN (без исполнения), AP = 0b011 (чтение и запись), TEX = 0b001 при
/// C = B = 0 — normal non-cacheable, S — общий с DMA, SIZE = log2(размер) - 1,
/// ENABLE.
const fn rasr(len: u32) -> u32 {
    (1 << 28) | (0b011 << 24) | (0b001 << 19) | (1 << 18) | ((len.trailing_zeros() - 1) << 1) | 1
}
