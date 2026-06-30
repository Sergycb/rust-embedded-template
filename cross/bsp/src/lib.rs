#![no_std]
#![no_main]

use defmt_rtt as _;

pub struct Board {
    p: embassy_stm32::Peripherals,
}

impl Board {
    pub fn init() -> Self {
        let p = embassy_stm32::init(embassy_stm32::Config::default());

        Self { p }
    }
}
