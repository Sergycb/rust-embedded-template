#![no_std]
#![no_main]

use defmt_rtt as _;
use embassy_boot_stm32::{AlignedBuffer, FirmwareUpdater, FirmwareUpdaterConfig};
use embassy_stm32::flash::{Flash, WRITE_SIZE};
use panic_probe as _;

#[cortex_m_rt::entry]
fn main() -> ! {
    let p = embassy_stm32::init(embassy_stm32::Config::default());
    let flash = Flash::new_blocking(p.FLASH);

    let config = FirmwareUpdaterConfig::from_linkerfile_blocking(&flash, &flash);
    let mut aligned = AlignedBuffer([0; WRITE_SIZE]);
    let mut updater = FirmwareUpdater::new(config, &mut aligned.0);

    if updater.get_state().unwrap() == embassy_boot::State::DfuDetach {
        updater.mark_booted().unwrap();
    }

    embassy_boot_stm32::BootLoader::prepare::<_, _, _, 2048>(
        FirmwareUpdaterConfig::from_linkerfile_blocking(&flash, &flash),
    );

    unreachable!()
}
