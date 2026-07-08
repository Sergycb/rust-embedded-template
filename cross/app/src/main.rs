#![no_std]
#![no_main]

#[cfg(not(any(feature = "debug", feature = "release")))]
compile_error!("either feature \"debug\" or \"release\" must be enabled");
#[cfg(all(feature = "debug", feature = "release"))]
compile_error!("features \"debug\" and \"release\" are mutually exclusive");

use shadow_rs::shadow;
shadow!(build);

mod task_orchestration;

use defmt_or_log::info;
#[cfg(feature = "debug")]
use defmt_rtt as _;
#[cfg(feature = "release")]
use panic_halt as _;
#[cfg(feature = "debug")]
use panic_probe as _;

use embassy_executor::Spawner;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    info!("app: starting");
    let mut _board = bsp::Board::init();
}
