#![doc = include_str!("../../../docs/modules/bsp-rng.md")]

use embassy_stm32::peripherals::RNG;
use embassy_stm32::{Peri, bind_interrupts, rng};

bind_interrupts!(struct Irqs {
    {{rng_irq}} => rng::InterruptHandler<RNG>;
});

/// Аппаратный генератор случайных чисел чипа (см. модуль).
pub struct Rng(rng::Rng<'static, RNG>);

impl Rng {
    /// Случайное 32-битное слово; ждёт, пока блок его выработает.
    pub fn next_u32(&mut self) -> u32 {
        self.0.next_u32()
    }

    /// Случайное 64-битное слово — два слова блока.
    pub fn next_u64(&mut self) -> u64 {
        self.0.next_u64()
    }

    /// Заполняет `dest` случайными байтами.
    pub fn fill_bytes(&mut self, dest: &mut [u8]) {
        self.0.fill_bytes(dest);
    }
}

// Вручную: у драйвера `embassy-stm32` нет `Debug`.
impl core::fmt::Debug for Rng {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Rng").finish_non_exhaustive()
    }
}

/// Включает блок. Клок ему уже дан конфигом HAL (`config()` в `bsp::board`).
pub(crate) fn new(rng: Peri<'static, RNG>) -> Rng {
    Rng(rng::Rng::new(rng, Irqs))
}
