//! Порт доступа к внутренней EEPROM (STM32L0/L1).
//!
//! У EEPROM нет стандартного трейта ни в `embedded-hal`, ни в
//! `embedded-storage`: реализует его `bsp` поверх
//! `embassy_stm32::flash::Flash::eeprom_*`, а `adapters::eeprom::Eeprom`
//! строит над ним `NorFlash` для хранилища настроек.

/// Побайтный доступ к EEPROM. Методы берут `&self`: реализация в `bsp` берёт
/// общий флеш под замком на каждую операцию.
pub trait EepromAccess {
    /// Отказ контроллера.
    type Error: core::fmt::Debug;

    /// Объём EEPROM в байтах.
    fn size(&self) -> u32;

    /// Читает `buf.len()` байт со смещения `offset`.
    ///
    /// # Errors
    ///
    /// Отказ контроллера или выход за границы.
    fn read(&self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error>;

    /// Пишет `data` со смещения `offset`, байт в байт.
    ///
    /// # Errors
    ///
    /// Отказ контроллера или выход за границы.
    fn write(&self, offset: u32, data: &[u8]) -> Result<(), Self::Error>;
}
