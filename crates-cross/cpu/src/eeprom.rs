#![doc = include_str!("../../../docs/modules/cpu-eeprom.md")]

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::RawMutex;
use embedded_storage::{ReadStorage, Storage};

use crate::flash::ChipFlash;

/// Внутренняя EEPROM как `NorFlash` — носитель настроек, над которым
/// стоит хранилище (`domain::settings`).
///
/// Флеш под настройки не тратится вовсе: EEPROM пишется через тот же
/// контроллер, что и флеш, поэтому доступ к ней — через общий
/// [`crate::flash::Lock`] (`embassy_stm32::flash::Flash::eeprom_*`), по
/// операции под замком. Обёртку `adapters_hw::eeprom::Eeprom` (инверсия байтов,
/// страница) собирает плата, и под хранилище отдана вся EEPROM, округлённая до
/// её страницы.
pub type Eeprom<M> = adapters_hw::eeprom::Eeprom<EepromFlash<M>>;

/// Общий замок флеша как источник EEPROM. Свой тип, а не `impl` на самом
/// мьютексе: и трейт `Storage`, и `Mutex` чужие для `cpu`.
pub struct EepromFlash<M: RawMutex + 'static>(&'static Mutex<M, RefCell<ChipFlash>>);

// Вручную: у `Flash` из `embassy-stm32` нет `Debug`.
impl<M: RawMutex + 'static> core::fmt::Debug for EepromFlash<M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EepromFlash").finish_non_exhaustive()
    }
}

impl<M: RawMutex + 'static> ReadStorage for EepromFlash<M> {
    type Error = embassy_stm32::flash::Error;

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.0
            .lock(|flash| flash.borrow_mut().eeprom_read_slice(offset, buf))
    }

    fn capacity(&self) -> usize {
        embassy_stm32::flash::EEPROM_SIZE
    }
}

impl<M: RawMutex + 'static> Storage for EepromFlash<M> {
    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        self.0
            .lock(|flash| flash.borrow_mut().eeprom_write_slice(offset, data))
    }
}

/// Собирает внутреннюю EEPROM как `NorFlash` поверх общего замка флеша.
#[must_use]
pub fn new<M: RawMutex + 'static>(lock: &'static Mutex<M, RefCell<ChipFlash>>) -> Eeprom<M> {
    adapters_hw::eeprom::Eeprom::new(EepromFlash(lock))
}
