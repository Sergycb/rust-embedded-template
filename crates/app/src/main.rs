#![no_std]
#![no_main]

use defmt as _;
use defmt_rtt as _;
use embassy_executor::Spawner;
use embassy_stm32 as _; // global logger + panicking-behavior + memory layout
#[cfg(test)]
use embedded_test as _;
use logic::ports::power_module::PowerModule as _;
#[cfg(not(test))]
use panic_probe as _;

#[cfg_attr(not(test), embassy_executor::main)]
async fn main(_spawner: Spawner) {
    let mut _board = bsp::Board::init();
}

#[cfg(test)]
#[embedded_test::tests]
mod tests {
    use super::*;

    #[init]
    fn init() -> Board {
        Board::init()
    }

    #[test]
    async fn test_works(_board: Board) {
        assert!(2 + 2 == 4);
    }
}
