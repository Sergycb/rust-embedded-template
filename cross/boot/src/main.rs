#![no_std]
#![no_main]

#[cfg(feature = "debug")]
use defmt_rtt as _;
#[cfg(feature = "debug")]
use panic_probe as _;
#[cfg(feature = "release")]
use panic_abort as _;

#[cortex_m_rt::entry]
fn main() {
    let p = embassy_stm32::init(embassy_stm32::Config::default());
}
