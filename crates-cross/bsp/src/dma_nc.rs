//! Память для буферов DMA при включённом кэше данных ядра Cortex-M7.
//!
//! DMA не видит кэш данных ядра: буфер, в который пишет периферия (Ethernet,
//! SDMMC, DMA-приёмник), читается из кэша устаревшим, а буфер, из которого она
//! читает, может не успеть выйти из кэша в RAM. Драйверы `embassy-stm32` кэш не
//! обслуживают. Поэтому у чипа с `AXISRAM` её хвост — регион `DMA_NC` в
//! `memory.x` — помечен в MPU как normal non-cacheable ([`protect`]), а буферы
//! DMA берутся из него ([`alloc_uninit`]). Кэш данных включается только после
//! этого (`bsp::cache`); чипам без `AXISRAM` (F7, H7R/S) он не включается вовсе.
//!
//! Границы региона — числа из генерации (`DMA_NC_START`, `DMA_NC_LEN` в `[env]`
//! `crates-cross/.cargo/config.toml`), а не символы линкера: `memory.x` считается
//! теми же цифрами. Секции в `memory.x` для региона нет намеренно: секция с VMA
//! вне RAM сбивает `flip-link` (см. комментарий к `PANIC` в `chip-select.rhai`),
//! поэтому память выдаётся не линкером, а [`alloc_uninit`].
//!
//! `AXISRAM` доступна DMA1/DMA2, MDMA, Ethernet и SDMMC1; периферии на BDMA
//! (SPI6, I2C4, LPUART1, ADC3) — только `SRAM4`, и сюда её буферы не годятся.

use core::mem::{MaybeUninit, align_of, size_of};
use core::sync::atomic::{AtomicUsize, Ordering};
use cortex_m::peripheral::MPU;

const START: usize = match usize::from_str_radix(env!("DMA_NC_START"), 10) {
    Ok(start) => start,
    Err(_) => panic!("DMA_NC_START в [env] crates-cross/.cargo/config.toml — не число"),
};
const LEN: usize = match usize::from_str_radix(env!("DMA_NC_LEN"), 10) {
    Ok(len) => len,
    Err(_) => panic!("DMA_NC_LEN в [env] crates-cross/.cargo/config.toml — не число"),
};
// Требование региона MPU Armv7-M: степень двойки не меньше 32 байт, адрес
// кратен размеру.
const _: () = assert!(
    LEN.is_power_of_two() && LEN >= 32 && START.is_multiple_of(LEN),
    "DMA_NC из генерации не годится для региона MPU: размер — степень двойки, адрес кратен размеру"
);

/// Номер региона MPU: последний из восьми. Младшие остаются проекту; перекрытий
/// с чужими регионами нет, а при перекрытии выигрывает старший номер.
const REGION: u32 = 7;
const CTRL_ENABLE: u32 = 1 << 0;
/// Привилегированному коду без регионов остаётся карта по умолчанию.
const CTRL_PRIVDEFENA: u32 = 1 << 2;
const RBAR_VALID: u32 = 1 << 4;
/// `RASR`: XN (без исполнения), AP = 0b011 (чтение и запись), TEX = 0b001 при
/// C = B = 0 — normal non-cacheable, S — общий с DMA, SIZE = log2(размер) - 1,
/// ENABLE.
const RASR: u32 =
    (1 << 28) | (0b011 << 24) | (0b001 << 19) | (1 << 18) | ((LEN.trailing_zeros() - 1) << 1) | 1;

/// Помечает `DMA_NC` в MPU non-cacheable и включает MPU. Зовётся один раз, из
/// `bsp::cache`, до включения кэша данных.
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
    let regions = (mpu._type.read() >> 8) & 0xFF;
    assert!(
        regions > REGION,
        "MPU: регионов меньше {}, DMA_NC нечем защитить",
        REGION + 1
    );
    #[expect(
        clippy::cast_possible_truncation,
        reason = "адрес в 32-битном пространстве Cortex-M"
    )]
    let rbar = START as u32 | RBAR_VALID | REGION;
    // SAFETY: зовётся из `Board::new` до запуска задач и прерываний; MPU на
    // время настройки выключен, затем включается с картой по умолчанию.
    unsafe {
        mpu.ctrl.write(0);
        cortex_m::asm::dmb();
        mpu.rbar.write(rbar);
        mpu.rasr.write(RASR);
        mpu.ctrl.write(CTRL_ENABLE | CTRL_PRIVDEFENA);
    }
    cortex_m::asm::dsb();
    cortex_m::asm::isb();
}

/// Смещение первого свободного байта в `DMA_NC`.
static NEXT: AtomicUsize = AtomicUsize::new(0);

/// Отдаёт место под `T` в `DMA_NC` — non-cacheable память под буфер DMA. Место
/// не освобождается: так берутся буферы драйверов, живущих всё время работы.
///
/// Память не обнулена (остаток прошлого запуска): инициализировать её —
/// `MaybeUninit::write`.
///
/// # Panics
///
/// Если в `DMA_NC` не осталось места под `T`.
#[expect(
    unsafe_code,
    reason = "ссылка на память региона, выданную ровно один раз"
)]
pub fn alloc_uninit<T>() -> &'static mut MaybeUninit<T> {
    let mut offset = 0;
    let reserved = NEXT.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |next| {
        // Смещение выравнивается относительно `START`, кратного `LEN` и потому
        // любому выравниванию, которое влезает в регион.
        let aligned = next.next_multiple_of(align_of::<T>());
        let end = aligned
            .checked_add(size_of::<T>())
            .filter(|&end| end <= LEN)?;
        offset = aligned;
        Some(end)
    });
    assert!(
        reserved.is_ok(),
        "DMA_NC исчерпан: нужно {} байт, регион — {LEN}",
        size_of::<T>()
    );
    // SAFETY: адрес внутри `DMA_NC`, выровнен под `T`, а `fetch_update` выдал
    // этот диапазон ровно одному вызову — ссылка единственная.
    unsafe { &mut *((START + offset) as *mut MaybeUninit<T>) }
}
