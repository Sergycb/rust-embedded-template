#![no_std]
#![no_main]

#[cfg(not(any(feature = "debug", feature = "release")))]
compile_error!("either feature \"debug\" or \"release\" must be enabled");
#[cfg(all(feature = "debug", feature = "release"))]
compile_error!("features \"debug\" and \"release\" are mutually exclusive");

use shadow_rs::shadow;
shadow!(build);

#[cfg(feature = "debug")]
use defmt_rtt as _;
#[cfg(feature = "release")]
use panic_abort as _;
#[cfg(feature = "debug")]
use panic_probe as _;

use embassy_executor::Spawner;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let mut _board = bsp::Board::init();
}
