//! Внутренняя EEPROM как флеш с семантикой NOR — чтобы поверх неё работало то
//! же хранилище настроек (`sequential-storage`), что и поверх флеша.
//!
//! У STM32L0/L1 есть настоящая EEPROM: пишется побайтно, без стирания, с
//! ресурсом на порядки выше флеша. Но `sequential-storage` ждёт `NorFlash`:
//! стёртое состояние `0xFF` и запись, которая только сбрасывает биты. Обёртка
//! даёт ровно это:
//!
//! - **Байты инвертированы.** Чистая EEPROM L0/L1 читается нулями (их флеш и
//!   EEPROM стираются в ноль), а хранилищу нужны `0xFF`. С инверсией заводской
//!   ноль читается как `0xFF` — свежая EEPROM сразу выглядит стёртой, без
//!   первичной подготовки.
//! - **Запись — побитовое И со старым значением**, как у NOR: сама EEPROM
//!   перезаписывает байт целиком, а `NorFlash` обещает, что запись только
//!   сбрасывает биты.
//! - **«Стирание» — тоже запись**, поэтому пишутся только изменившиеся слова:
//!   запись слова в EEPROM L0 — около 3 мс, и всё это время исполнитель стоит
//!   (доступ идёт под блокирующим замком общего флеша). Стирание уже чистой
//!   страницы не стоит ничего, грязной — по слову на каждое изменившееся.
//!
//! Доступ к самой EEPROM — трейт [`EepromAccess`](crate::eeprom::EepromAccess): его реализует `bsp` поверх
//! `embassy_stm32::flash::Flash::eeprom_*` (того же объекта, что делят OTA и
//! настройки), host-тест — поверх памяти.

use embedded_storage::nor_flash::{ErrorType, NorFlashError, NorFlashErrorKind};

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

/// Ошибка обёртки: выход за границы, невыровненный диапазон или отказ самой
/// EEPROM.
#[derive(Debug)]
pub enum Error<E> {
    /// Диапазон за пределами EEPROM.
    OutOfBounds,
    /// Диапазон не выровнен по странице или слову.
    NotAligned,
    /// Отказ контроллера EEPROM.
    Device(E),
}

impl<E: core::fmt::Debug> NorFlashError for Error<E> {
    fn kind(&self) -> NorFlashErrorKind {
        match self {
            Self::OutOfBounds => NorFlashErrorKind::OutOfBounds,
            Self::NotAligned => NorFlashErrorKind::NotAligned,
            Self::Device(_) => NorFlashErrorKind::Other,
        }
    }
}

/// EEPROM, видимая как `NorFlash` со страницей [`Eeprom::PAGE`] байт.
#[derive(Debug)]
pub struct Eeprom<'a, A> {
    eeprom: &'a A,
}

impl<'a, A: EepromAccess> Eeprom<'a, A> {
    /// Страница «стирания». Своей у EEPROM нет; 128 байт — компромисс:
    /// `sequential-storage` нужны минимум две страницы, и под хранилище
    /// годится EEPROM от 256 байт (у STM32L010F4/K4 её всего 128 — им хук
    /// EEPROM не предлагает, см. `MIN_SIZE`); полное стирание страницы — 32
    /// записи слова, около 0,1 с у L0.
    pub const PAGE: u32 = 128;

    /// Меньше двух страниц `sequential-storage` не берёт.
    pub const MIN_SIZE: u32 = 2 * Self::PAGE;

    /// Оборачивает доступ к EEPROM.
    #[must_use]
    pub const fn new(eeprom: &'a A) -> Self {
        Self { eeprom }
    }

    /// Объём, пригодный под хранилище: вся EEPROM, округлённая вниз до
    /// страницы.
    #[must_use]
    pub fn usable(&self) -> u32 {
        self.eeprom.size() / Self::PAGE * Self::PAGE
    }

    fn check(&self, offset: u32, len: usize, unit: u32) -> Result<(), Error<A::Error>> {
        let len = u32::try_from(len).ok().ok_or(Error::OutOfBounds)?;
        let end = offset.checked_add(len).ok_or(Error::OutOfBounds)?;
        if end > self.usable() {
            return Err(Error::OutOfBounds);
        }
        if !offset.is_multiple_of(unit) || !len.is_multiple_of(unit) {
            return Err(Error::NotAligned);
        }
        Ok(())
    }

    /// Переписывает выровненный по слову диапазон: `next(сдвиг, сырые байты)`
    /// превращает прочитанный кусок в то, что должно лечь, и пишутся только
    /// изменившиеся слова — каждое стоит миллисекунды.
    fn rewrite(
        &self,
        offset: u32,
        len: u32,
        mut next: impl FnMut(usize, &mut [u8]),
    ) -> Result<(), Error<A::Error>> {
        /// Сколько байт читается за раз.
        const CHUNK: u32 = 32;
        /// Слово записи EEPROM.
        const WORD: usize = 4;
        let mut done = 0;
        while done < len {
            let size = CHUNK.min(len - done) as usize;
            let mut old = [0; CHUNK as usize];
            let old = &mut old[..size];
            self.eeprom
                .read(offset + done, old)
                .map_err(Error::Device)?;
            let mut new = [0; CHUNK as usize];
            let new = &mut new[..size];
            new.copy_from_slice(old);
            next(done as usize, new);
            for (index, (word, was)) in new.chunks(WORD).zip(old.chunks(WORD)).enumerate() {
                if word != was {
                    let at = offset
                        + done
                        + u32::try_from(index * WORD).ok().ok_or(Error::OutOfBounds)?;
                    self.eeprom.write(at, word).map_err(Error::Device)?;
                }
            }
            done += CHUNK;
        }
        Ok(())
    }
}

impl<A: EepromAccess> ErrorType for Eeprom<'_, A> {
    type Error = Error<A::Error>;
}

impl<A: EepromAccess> embedded_storage_async::nor_flash::ReadNorFlash for Eeprom<'_, A> {
    const READ_SIZE: usize = 1;

    fn read(
        &mut self,
        offset: u32,
        bytes: &mut [u8],
    ) -> impl Future<Output = Result<(), Self::Error>> {
        let result = self.check(offset, bytes.len(), 1).and_then(|()| {
            self.eeprom.read(offset, bytes).map_err(Error::Device)?;
            for byte in bytes.iter_mut() {
                *byte = !*byte;
            }
            Ok(())
        });
        core::future::ready(result)
    }

    fn capacity(&self) -> usize {
        self.usable() as usize
    }
}

impl<A: EepromAccess> embedded_storage_async::nor_flash::NorFlash for Eeprom<'_, A> {
    /// Слово: хранилище пишет кусками этой кратности, а EEPROM словом
    /// записывает быстрее, чем побайтно.
    const WRITE_SIZE: usize = 4;
    const ERASE_SIZE: usize = Self::PAGE as usize;

    /// Сырой ноль — это логическое `0xFF`, то есть «стёрто».
    fn erase(&mut self, from: u32, to: u32) -> impl Future<Output = Result<(), Self::Error>> {
        let result = to
            .checked_sub(from)
            .ok_or(Error::OutOfBounds)
            .and_then(|len| {
                self.check(from, len as usize, Self::PAGE)?;
                self.rewrite(from, len, |_, raw| raw.fill(0))
            });
        core::future::ready(result)
    }

    /// Логически новое = старое И записываемое; в сырых (инвертированных)
    /// байтах это старое ИЛИ НЕ записываемое.
    fn write(
        &mut self,
        offset: u32,
        bytes: &[u8],
    ) -> impl Future<Output = Result<(), Self::Error>> {
        let result = self.check(offset, bytes.len(), 4).and_then(|()| {
            let len = u32::try_from(bytes.len()).ok().ok_or(Error::OutOfBounds)?;
            self.rewrite(offset, len, |done, raw| {
                for (cell, byte) in raw.iter_mut().zip(&bytes[done..]) {
                    *cell |= !*byte;
                }
            })
        });
        core::future::ready(result)
    }
}

#[cfg(test)]
mod tests {
    use core::cell::RefCell;

    use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};

    use super::{Eeprom, EepromAccess};
    use crate::settings::Settings;

    /// EEPROM в памяти: с завода нули, как у L0/L1; считает записи.
    #[derive(Debug)]
    struct MemEeprom {
        mem: RefCell<Vec<u8>>,
        writes: RefCell<usize>,
    }

    impl MemEeprom {
        fn new(size: usize) -> Self {
            Self {
                mem: RefCell::new(vec![0; size]),
                writes: RefCell::new(0),
            }
        }
    }

    impl EepromAccess for MemEeprom {
        type Error = ();

        fn size(&self) -> u32 {
            u32::try_from(self.mem.borrow().len()).unwrap()
        }

        fn read(&self, offset: u32, buf: &mut [u8]) -> Result<(), ()> {
            let start = offset as usize;
            buf.copy_from_slice(&self.mem.borrow()[start..start + buf.len()]);
            Ok(())
        }

        fn write(&self, offset: u32, data: &[u8]) -> Result<(), ()> {
            let start = offset as usize;
            self.mem.borrow_mut()[start..start + data.len()].copy_from_slice(data);
            *self.writes.borrow_mut() += 1;
            Ok(())
        }
    }

    /// Заводские нули читаются стёртым флешем, и стирание чистого ничего не
    /// пишет.
    #[tokio::test]
    async fn fresh_eeprom_reads_as_erased_flash() {
        let eeprom = MemEeprom::new(512);
        let mut flash = Eeprom::new(&eeprom);

        let mut back = [0; 16];
        flash.read(0, &mut back).await.expect("чтение");
        assert_eq!(back, [0xFF; 16]);

        flash.erase(0, 512).await.expect("стирание");
        assert_eq!(*eeprom.writes.borrow(), 0, "стирать чистое незачем");
    }

    /// Пишется только изменившееся слово, а не весь кусок: запись слова в
    /// EEPROM — миллисекунды, и всё это время исполнитель стоит.
    #[tokio::test]
    async fn only_changed_words_are_written() {
        let eeprom = MemEeprom::new(512);
        let mut flash = Eeprom::new(&eeprom);

        flash
            .write(8, &[0x12, 0x34, 0x56, 0x78])
            .await
            .expect("запись");
        assert_eq!(*eeprom.writes.borrow(), 1, "одно слово");

        flash.erase(0, 128).await.expect("стирание");
        assert_eq!(
            *eeprom.writes.borrow(),
            2,
            "стирание тронуло одно грязное слово"
        );
    }

    /// Запись — побитовое И, как у NOR, а стирание возвращает `0xFF`.
    #[tokio::test]
    async fn writes_behave_like_nor_flash() {
        let eeprom = MemEeprom::new(512);
        let mut flash = Eeprom::new(&eeprom);

        flash
            .write(128, &[0xF0, 0xFF, 0x0F, 0xAA])
            .await
            .expect("запись");
        flash
            .write(128, &[0x3C, 0x00, 0xFF, 0xFF])
            .await
            .expect("повторная запись");
        let mut back = [0; 4];
        flash.read(128, &mut back).await.expect("чтение");
        assert_eq!(back, [0x30, 0x00, 0x0F, 0xAA]);

        flash.erase(128, 256).await.expect("стирание");
        flash.read(128, &mut back).await.expect("чтение");
        assert_eq!(back, [0xFF; 4]);
    }

    #[tokio::test]
    async fn refuses_unaligned_and_out_of_bounds_ranges() {
        let eeprom = MemEeprom::new(512);
        let mut flash = Eeprom::new(&eeprom);

        assert!(flash.erase(0, 100).await.is_err(), "не по странице");
        assert!(flash.write(2, &[0; 4]).await.is_err(), "не по слову");
        assert!(flash.write(512, &[0; 4]).await.is_err(), "за границей");
        assert_eq!(flash.capacity(), 512);
    }

    /// Главное: хранилище настроек работает поверх EEPROM с завода, без
    /// первичной подготовки.
    #[tokio::test]
    async fn settings_work_on_a_fresh_eeprom() {
        use ports::SettingsStorage;

        let eeprom = MemEeprom::new(512);
        let mut settings = Settings::new(Eeprom::new(&eeprom), 0..512);
        let mut scratch = [0; 64];

        settings.write(1, b"template").await.expect("запись");
        settings.write(2, b"second").await.expect("запись");

        assert_eq!(
            settings.read(1, &mut scratch).await.expect("чтение"),
            Some(&b"template"[..])
        );
        assert_eq!(
            settings.read(2, &mut scratch).await.expect("чтение"),
            Some(&b"second"[..])
        );
    }
}
