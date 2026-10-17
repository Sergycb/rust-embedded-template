#![doc = include_str!("../../../docs/modules/bsp-rng.md")]

use embassy_stm32::peripherals::RNG;
use embassy_stm32::{Peri, bind_interrupts, rng};

bind_interrupts!(struct Irqs {
    {{rng_irq}} => rng::InterruptHandler<RNG>;
});

/// Первое случайное слово блока — затравка сетевого стека. Блок включается
/// на время вызова и выключается, когда `Rng` дропается: клок ему уже дан
/// конфигом HAL (`config()` в `bsp::board`).
pub(crate) fn seed(rng: Peri<'static, RNG>) -> u64 {
    rng::Rng::new(rng, Irqs).next_u64()
}
