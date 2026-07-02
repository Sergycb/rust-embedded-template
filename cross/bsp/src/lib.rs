#![no_std]

pub struct Board {
    p: embassy_stm32::Peripherals,
}

impl Board {
    pub fn init() -> Self {
        let p = embassy_stm32::init(embassy_stm32::Config::default());

        Self { p }
    }
}
