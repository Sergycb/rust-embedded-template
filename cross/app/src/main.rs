#![no_std]
#![no_main]

#[cfg(feature = "debug")]
use defmt as _;
#[cfg(feature = "debug")]
use defmt_rtt as _;
use embassy_executor::Spawner;

#[cfg(feature = "debug")]
use panic_probe as _;
#[cfg(feature = "release")]
use panic_abort as _;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    let mut _board = bsp::Board::init();
}
