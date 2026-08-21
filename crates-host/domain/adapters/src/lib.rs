#![cfg_attr(not(test), no_std)]

//! Адаптеры — реализации портов из `ports`.
//!
//! Здесь живёт то, что можно реализовать без железа: фейки и симуляторы для
//! host-тестов, пересчёты, обёртки над чужими протоколами. Реализация поверх
//! настоящей периферии — тоже адаптер, но его место зависит от того, что ей
//! нужно: пока хватает трейтов `embedded-hal` (шина передаётся параметром) —
//! она может лежать здесь же и оставаться тестируемой на хосте через
//! `embedded-hal-mock`; как только понадобится `embassy_stm32::peripherals`
//! или статический DMA-буфер — адаптер переезжает в `crates-cross/bsp`, потому что
//! это уже не логика, а железо (см. CLAUDE.md, «Главный принцип»).

use ports::TemperatureSensor;

/// Датчик, всегда отдающий одно и то же значение.
///
/// Нужен не «для полноты», а чтобы host-тесты логики не ждали железа: любой
/// сценарий, где важна реакция на температуру, собирается из этого фейка и
/// проверяется обычным `cargo xtask test host`.
pub struct FixedTemperature {
    millicelsius: i32,
}

impl FixedTemperature {
    pub const fn new(millicelsius: i32) -> Self {
        Self { millicelsius }
    }
}

/// Отказать этот датчик не может — отсюда `Infallible` вместо выдуманного
/// типа ошибки: вызывающий код видит по сигнатуре, что обрабатывать нечего.
impl TemperatureSensor for FixedTemperature {
    type Error = core::convert::Infallible;

    fn read_millicelsius(&mut self) -> Result<i32, Self::Error> {
        Ok(self.millicelsius)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fixed_sensor_reports_what_it_was_given() {
        let mut sensor = FixedTemperature::new(25_500);
        assert_eq!(sensor.read_millicelsius(), Ok(25_500));
    }
}
