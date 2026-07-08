#![no_std]
#![no_main]

#[cfg(not(any(feature = "debug", feature = "release")))]
compile_error!("either feature \"debug\" or \"release\" must be enabled");
#[cfg(all(feature = "debug", feature = "release"))]
compile_error!("features \"debug\" and \"release\" are mutually exclusive");

use core::cell::RefCell;

use defmt_or_log::info;
#[cfg(feature = "debug")]
use defmt_rtt as _;
use embassy_boot_stm32::{BootLoader, BootLoaderConfig};
use embassy_stm32::flash::{BANK1_REGION, Flash};
use embassy_sync::blocking_mutex::Mutex;
#[cfg(feature = "release")]
use panic_halt as _;
#[cfg(feature = "debug")]
use panic_probe as _;

#[cortex_m_rt::entry]
fn main() -> ! {
    let p = embassy_stm32::init(embassy_stm32::Config::default());

    let layout = Flash::new_blocking(p.FLASH).into_blocking_regions();
    let flash = Mutex::new(RefCell::new(layout.bank1_region));

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &flash);
    let active_offset = config.active.offset();
    // Последний параметр (WRITE_SIZE) — размер страницы/блока flash в байтах
    // для операций bootloader'а; зависит от чипа (у STM32F1/F3/L4/G4 — как
    // правило 2048, у STM32F4/F7/H7 сектора крупнее). Подставьте значение
    // для вашего {{chip_feature}} перед первой прошивкой.
    let bl = BootLoader::prepare::<_, _, _, 2048>(config);

    let entry = BANK1_REGION.base() + active_offset;
    info!("boot: jumping to app at {:x}", entry);
    unsafe { bl.load(entry) }
}
