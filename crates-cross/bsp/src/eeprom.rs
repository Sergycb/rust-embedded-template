#![doc = include_str!("../../../docs/modules/bsp-eeprom.md")]

use embedded_storage::{ReadStorage, Storage};

use crate::flash::Lock;

/// Внутренняя EEPROM как `NorFlash` — носитель настроек, над которым
/// стоит хранилище (`domain::settings`).
///
/// Флеш под настройки не тратится вовсе: EEPROM пишется через тот же
/// контроллер, что и флеш, поэтому доступ к ней — через общий [`Lock`]
/// (`embassy_stm32::flash::Flash::eeprom_*`), по операции под замком. Обёртку
/// `adapters_hw::eeprom::Eeprom` (инверсия байтов, страница) собирает плата, и
/// под хранилище отдана вся EEPROM, округлённая до её страницы.
pub type ConfigFlash = adapters_hw::eeprom::Eeprom<EepromFlash>;

/// Общий [`Lock`] как источник EEPROM. Свой тип, а не `impl` на самом
/// мьютексе: и трейт `Storage`, и `Mutex` чужие для `bsp`.
pub struct EepromFlash(&'static Lock);

// Вручную: у `Flash` из `embassy-stm32` нет `Debug`.
impl core::fmt::Debug for EepromFlash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EepromFlash").finish_non_exhaustive()
    }
}

impl ReadStorage for EepromFlash {
    type Error = embassy_stm32::flash::Error;

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.0
            .lock(|flash| flash.borrow_mut().eeprom_read_slice(offset, buf))
    }

    fn capacity(&self) -> usize {
        embassy_stm32::flash::EEPROM_SIZE
    }
}

impl Storage for EepromFlash {
    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        self.0
            .lock(|flash| flash.borrow_mut().eeprom_write_slice(offset, data))
    }
}

/// Собирает внутреннюю EEPROM как `NorFlash`.
pub(crate) fn config(flash: &'static Lock) -> ConfigFlash {
    adapters_hw::eeprom::Eeprom::new(EepromFlash(flash))
}
