#![no_std]

pub mod buffers;
pub mod resources;

use defmt_or_log::info;

pub struct Board {
    // Not yet split into individual peripherals; kept whole until board wiring is added.
    #[allow(dead_code)]
    p: embassy_stm32::Peripherals,
}

impl Board {
    pub fn init() -> Self {
        let p = embassy_stm32::init(embassy_stm32::Config::default());
        info!("bsp: board initialized");

        Self { p }
    }
}
