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
//! Границы региона — числа из генерации (`DMA_NC_START`, `DMA_NC_LEN`,
//! `BDMA_NC_START`, `BDMA_NC_LEN` в `[env]` `crates-cross/.cargo/config.toml`),
//! а не символы линкера: `memory.x` считается теми же цифрами.

use cortex_m::peripheral::MPU;

const fn number(text: &str) -> u32 {
    match u32::from_str_radix(text, 10) {
        Ok(value) => value,
        Err(_) => panic!("границы DMA_NC в [env] crates-cross/.cargo/config.toml — не число"),
    }
}

/// Регионы MPU, по порядку: `(адрес, размер)`; первому достаётся [`FIRST_REGION`],
/// каждому следующему — на единицу меньше.
const REGIONS: &[(u32, u32)] = &[DMA_NC{% if bdma_nc_start != "" %}, BDMA_NC{% endif %}];

/// `DMA_NC` — буферы DMA1/DMA2, MDMA, Ethernet, SDMMC.
const DMA_NC: (u32, u32) = (number(env!("DMA_NC_START")), number(env!("DMA_NC_LEN")));
{%- if bdma_nc_start != "" %}
/// `SRAM4` целиком — буферы BDMA.
const BDMA_NC: (u32, u32) = (number(env!("BDMA_NC_START")), number(env!("BDMA_NC_LEN")));
{%- endif %}

/// Номер региона MPU первого региона: старшие номера остаются проекту
/// (выше номер — выше приоритет при перекрытии, но перекрытий здесь нет).
const FIRST_REGION: u32 = 7;
const CTRL_ENABLE: u32 = 1 << 0;
/// Привилегированному коду без регионов остаётся карта по умолчанию.
const CTRL_PRIVDEFENA: u32 = 1 << 2;
const RBAR_VALID: u32 = 1 << 4;

/// `RASR`: XN (без исполнения), AP = 0b011 (чтение и запись), TEX = 0b001 при
/// C = B = 0 — normal non-cacheable, S — общий с DMA, SIZE = log2(размер) - 1,
/// ENABLE.
const fn rasr(len: u32) -> u32 {
    (1 << 28) | (0b011 << 24) | (0b001 << 19) | (1 << 18) | ((len.trailing_zeros() - 1) << 1) | 1
}

// Требование региона MPU Armv7-M: размер — степень двойки не меньше 32 байт,
// адрес кратен размеру; регионов в MPU хватает.
const _: () = {
    let mut i = 0;
    while i < REGIONS.len() {
        let (start, len) = REGIONS[i];
        assert!(
            len.is_power_of_two() && len >= 32 && start.is_multiple_of(len),
            "регион DMA_NC из генерации не годится для MPU: размер — степень двойки, адрес кратен размеру"
        );
        i += 1;
    }
    // Номера идут от `FIRST_REGION` вниз до нуля: регионов не больше восьми.
    assert!(REGIONS.len() <= 8);
};

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
        for &(start, len) in REGIONS {
            mpu.rbar.write(start | RBAR_VALID | region);
            mpu.rasr.write(rasr(len));
            region -= 1;
        }
        mpu.ctrl.write(CTRL_ENABLE | CTRL_PRIVDEFENA);
    }
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}
