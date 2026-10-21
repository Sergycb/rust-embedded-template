#![doc = include_str!("../../../docs/modules/bsp-rng.md")]

use embassy_stm32::peripherals::RNG;
use embassy_stm32::{Peri, bind_interrupts, rng};

bind_interrupts!(struct Irqs {
    {{rng_irq}} => rng::InterruptHandler<RNG>;
});

/// Генератор случайных чисел платы. Блок включён, пока объект жив, и
/// выключается, когда его дропают: клок ему дан конфигом HAL (`config()` в
/// `bsp::board`).
pub type Rng = rng::Rng<'static, RNG>;

/// Собирает генератор.
pub(crate) fn new(rng: Peri<'static, RNG>) -> Rng {
    Rng::new(rng, Irqs)
}
