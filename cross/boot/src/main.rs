#![no_std]
#![no_main]

#[cfg(not(any(feature = "debug", feature = "release")))]
compile_error!("either feature \"debug\" or \"release\" must be enabled");
#[cfg(all(feature = "debug", feature = "release"))]
compile_error!("features \"debug\" and \"release\" are mutually exclusive");

#[cfg(feature = "debug")]
use defmt_rtt as _;
#[cfg(feature = "release")]
use panic_abort as _;
#[cfg(feature = "debug")]
use panic_probe as _;

#[cortex_m_rt::entry]
fn main() -> ! {
    let _p = embassy_stm32::init(embassy_stm32::Config::default());

    loop {
        cortex_m::asm::wfi();
    }
}
