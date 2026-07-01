#![no_std]
#![no_main]

use shadow_rs::shadow;
shadow!(build);

#[cfg(feature = "debug")]
use defmt_rtt as _;
#[cfg(feature = "debug")]
use panic_probe as _;
#[cfg(feature = "release")]
use panic_abort as _;

use embassy_executor::Spawner;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let mut _board = bsp::Board::init();
}
