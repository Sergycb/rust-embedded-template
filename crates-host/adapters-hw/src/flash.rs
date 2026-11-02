//! Флеш с семантикой NOR без чипа.

/// Флеш в памяти с семантикой NOR — для тестов разделов OTA и хранилища
/// настроек. Готового мока `NorFlash` в экосистеме нет (`embedded-hal-mock`
/// флеш не покрывает), поэтому фейк свой. Только на хосте: ему нужен `alloc`.
#[cfg(all(any(test, feature = "fake"), not(target_os = "none")))]
pub mod fake {
    extern crate alloc;

    use alloc::vec;
    use alloc::vec::Vec;

    use embedded_storage::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};

    /// Флеш на `SIZE` байт: стирание страницами по `ERASE` в `0xFF`, запись
    /// словами по `WRITE` и только «единицы в нули», как у настоящего NOR, —
    /// иначе не проверить, что пишущий не пишет поверх нестёртого. Невыровненные
    /// и выходящие за `SIZE` операции — отказ, как на чипе: без этих проверок
    /// тест не отличил бы буферизацию по слову от её отсутствия.
    ///
    /// Умеет и блокирующий `NorFlash` (проверка образа в bootloader'е), и
    /// асинхронный (`Updater`, `sequential-storage`) — второй поверх первого,
    /// готов с первого `poll`.
    #[derive(Debug)]
    pub struct MemFlash<const SIZE: usize, const WRITE: usize, const ERASE: usize> {
        /// Содержимое; длина — `SIZE`. Открыто, чтобы тест мог подложить
        /// любое содержимое (мусор, «прошлый запуск») и заглянуть в записанное.
        pub mem: Vec<u8>,
    }

    impl<const SIZE: usize, const WRITE: usize, const ERASE: usize> MemFlash<SIZE, WRITE, ERASE> {
        /// Стёртый флеш: всё в `0xFF`.
        #[must_use]
        pub fn erased() -> Self {
            Self::filled(0xFF)
        }

        /// Флеш, заполненный байтом `byte`.
        #[must_use]
        pub fn filled(byte: u8) -> Self {
            Self {
                mem: vec![byte; SIZE],
            }
        }

        fn check(offset: u32, len: usize, unit: usize) -> Result<(), NorFlashErrorKind> {
            if offset as usize + len > SIZE {
                return Err(NorFlashErrorKind::OutOfBounds);
            }
            if !(offset as usize).is_multiple_of(unit) || !len.is_multiple_of(unit) {
                return Err(NorFlashErrorKind::NotAligned);
            }
            Ok(())
        }
    }

    impl<const SIZE: usize, const WRITE: usize, const ERASE: usize> ErrorType
        for MemFlash<SIZE, WRITE, ERASE>
    {
        type Error = NorFlashErrorKind;
    }

    impl<const SIZE: usize, const WRITE: usize, const ERASE: usize> ReadNorFlash
        for MemFlash<SIZE, WRITE, ERASE>
    {
        const READ_SIZE: usize = 1;

        fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            Self::check(offset, bytes.len(), 1)?;
            let start = offset as usize;
            bytes.copy_from_slice(&self.mem[start..start + bytes.len()]);
            Ok(())
        }

        fn capacity(&self) -> usize {
            SIZE
        }
    }

    impl<const SIZE: usize, const WRITE: usize, const ERASE: usize> NorFlash
        for MemFlash<SIZE, WRITE, ERASE>
    {
        const WRITE_SIZE: usize = WRITE;
        const ERASE_SIZE: usize = ERASE;

        fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            Self::check(from, (to - from) as usize, ERASE)?;
            self.mem[from as usize..to as usize].fill(0xFF);
            Ok(())
        }

        fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            Self::check(offset, bytes.len(), WRITE)?;
            for (cell, byte) in self.mem[offset as usize..].iter_mut().zip(bytes) {
                *cell &= *byte;
            }
            Ok(())
        }
    }

    impl<const SIZE: usize, const WRITE: usize, const ERASE: usize>
        embedded_storage_async::nor_flash::ReadNorFlash for MemFlash<SIZE, WRITE, ERASE>
    {
        const READ_SIZE: usize = 1;

        fn read(
            &mut self,
            offset: u32,
            bytes: &mut [u8],
        ) -> impl Future<Output = Result<(), Self::Error>> {
            core::future::ready(ReadNorFlash::read(self, offset, bytes))
        }

        fn capacity(&self) -> usize {
            SIZE
        }
    }

    impl<const SIZE: usize, const WRITE: usize, const ERASE: usize>
        embedded_storage_async::nor_flash::NorFlash for MemFlash<SIZE, WRITE, ERASE>
    {
        const WRITE_SIZE: usize = WRITE;
        const ERASE_SIZE: usize = ERASE;

        fn erase(&mut self, from: u32, to: u32) -> impl Future<Output = Result<(), Self::Error>> {
            core::future::ready(NorFlash::erase(self, from, to))
        }

        fn write(
            &mut self,
            offset: u32,
            bytes: &[u8],
        ) -> impl Future<Output = Result<(), Self::Error>> {
            core::future::ready(NorFlash::write(self, offset, bytes))
        }
    }
}
