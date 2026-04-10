#![no_std]
#![no_main]

use core::cell::RefCell;

use defmt_rtt as _;
use embassy_boot::BootLoaderConfig;
use embassy_boot_stm32::{BootLoader};
use embassy_stm32::flash::{BANK1_REGION, Flash};
use embassy_sync::blocking_mutex::Mutex;
#[cfg(test)]
use embedded_test as _;
#[cfg(not(test))]
use panic_probe as _;

#[cortex_m_rt::entry]
fn main() -> ! {
    let p = embassy_stm32::init(embassy_stm32::Config::default());
    let layout = Flash::new_blocking(p.FLASH).into_blocking_regions();
    let flash = Mutex::new(RefCell::new(layout.bank1_region));

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &flash);
    let active_offset = config.active.offset();
    let bl = BootLoader::prepare::<_, _, _, 2048>(config);

    unsafe { bl.load(BANK1_REGION.base() + active_offset) }
}

#[cfg(test)]
#[embedded_test::tests]
mod tests {
    use super::*;

    #[init]
    fn init() -> () {
        ()
    }

    #[test]
    fn test_works() {
        assert!(true);
    }
}
