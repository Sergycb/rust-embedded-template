//! Флеш в памяти для host-тестов адаптеров: ведёт себя как NOR — стирание в
//! `0xFF`, запись только словами и только «единицы в нули» (побитовое И).

use embedded_storage::nor_flash::{
    ErrorType, NorFlash, NorFlashError, NorFlashErrorKind, ReadNorFlash,
};

pub struct MemFlash<const SIZE: usize, const ERASE: usize, const WRITE: usize> {
    pub mem: Vec<u8>,
}

#[derive(Debug)]
pub struct MemFlashError(pub NorFlashErrorKind);

impl NorFlashError for MemFlashError {
    fn kind(&self) -> NorFlashErrorKind {
        self.0
    }
}

impl<const SIZE: usize, const ERASE: usize, const WRITE: usize> MemFlash<SIZE, ERASE, WRITE> {
    pub fn erased() -> Self {
        Self {
            mem: vec![0xFF; SIZE],
        }
    }

    pub fn filled(byte: u8) -> Self {
        Self {
            mem: vec![byte; SIZE],
        }
    }

    fn check(offset: u32, len: usize, unit: usize) -> Result<(), MemFlashError> {
        if offset as usize + len > SIZE {
            return Err(MemFlashError(NorFlashErrorKind::OutOfBounds));
        }
        if !(offset as usize).is_multiple_of(unit) || !len.is_multiple_of(unit) {
            return Err(MemFlashError(NorFlashErrorKind::NotAligned));
        }
        Ok(())
    }
}

impl<const SIZE: usize, const ERASE: usize, const WRITE: usize> ErrorType
    for MemFlash<SIZE, ERASE, WRITE>
{
    type Error = MemFlashError;
}

impl<const SIZE: usize, const ERASE: usize, const WRITE: usize> ReadNorFlash
    for MemFlash<SIZE, ERASE, WRITE>
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

impl<const SIZE: usize, const ERASE: usize, const WRITE: usize> NorFlash
    for MemFlash<SIZE, ERASE, WRITE>
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

// Тот же фейк — под асинхронные трейты, которых ждёт `sequential-storage`.
impl<const SIZE: usize, const ERASE: usize, const WRITE: usize>
    embedded_storage_async::nor_flash::ReadNorFlash for MemFlash<SIZE, ERASE, WRITE>
{
    const READ_SIZE: usize = 1;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        ReadNorFlash::read(self, offset, bytes)
    }

    fn capacity(&self) -> usize {
        SIZE
    }
}

impl<const SIZE: usize, const ERASE: usize, const WRITE: usize>
    embedded_storage_async::nor_flash::NorFlash for MemFlash<SIZE, ERASE, WRITE>
{
    const WRITE_SIZE: usize = WRITE;
    const ERASE_SIZE: usize = ERASE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        NorFlash::erase(self, from, to)
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        NorFlash::write(self, offset, bytes)
    }
}

/// Один `poll` — фейки не ждут.
pub fn block_on<T>(future: impl core::future::Future<Output = T>) -> T {
    use core::task::{Context, Poll, Waker};
    let mut future = core::pin::pin!(future);
    match future
        .as_mut()
        .poll(&mut Context::from_waker(Waker::noop()))
    {
        Poll::Ready(value) => value,
        Poll::Pending => panic!("фейк флеша не ждёт: future не должна возвращать Pending"),
    }
}
