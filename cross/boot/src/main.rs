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
    let bl = BootLoader::prepare::<_, _, _, {{write_size}}>(config);

    // Минимальный bootloader: как и официальный пример embassy-boot-stm32,
    // прыгает в активный образ безусловно, не проверяя валидность вектора
    // сброса/SP — повреждённый образ (прерванная прошивка, битый DFU) даст
    // HardFault вместо отказа с диагностикой. Полная проверка целостности
    // требует chip-specific границ RAM и не входит в минимальный шаблон.
    let entry = BANK1_REGION.base() + active_offset;
    info!("boot: jumping to app at {:x}", entry);
    unsafe { bl.load(entry) }
}
