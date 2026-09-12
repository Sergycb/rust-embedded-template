//! Настройки во flash: пары «ключ — значение», переживающие и перезапуск, и
//! обновление прошивки, — поверх `sequential-storage`.
//!
//! Раздел под них режет генерация (`CONFIG`, две последние страницы стирания
//! — меньше `sequential-storage` не берёт), а границы приносит `bsp::config`
//! диапазоном; самому хранилищу всё равно, что за флеш под ним, — оттого оно
//! здесь и тестируется на хосте с флешем в памяти. Чем сериализовать значение
//! (postcard, `miniconf`, свой формат), решает проект.
//!
//! ```ignore
//! // Чтение и запись живут в порту домена — без импорта методов не видно.
//! use ports::SettingsStorage;
//!
//! let mut scratch = [0u8; 64];
//! board.settings.write(KEY_CALIBRATION, &postcard::to_slice(&cal, &mut buf)?).await?;
//! if let Some(raw) = board.settings.read(KEY_CALIBRATION, &mut scratch).await? {
//!     let cal: Calibration = postcard::from_bytes(raw)?;
//! }
//! ```
//!
//! # Чего здесь нет
//!
//! **Удаления ключа.** `sequential_storage::map::remove_item` требует
//! [`MultiwriteNorFlash`](embedded_storage::nor_flash::MultiwriteNorFlash) —
//! флеша, в одно и то же слово которого можно писать дважды. У STM32 это не
//! так, и трейт для `Flash` не реализован. Ключ «удаляется» записью значения,
//! которое ваш формат понимает как «нет данных» (`Option::None` в postcard —
//! один байт).
//!
//! **Кеша.** `Cache::new_uncached()`: кеш ускоряет поиск, но требует, чтобы
//! он был либо новым, либо в точности соответствующим содержимому флеша, —
//! а раздел переживает перезапуск и обновление прошивки, то есть «в точности
//! соответствующий» гарантировать нечем. Настройки читаются на старте и
//! пишутся редко, экономить здесь нечего.

use core::ops::Range;

use embedded_storage_async::nor_flash::NorFlash;
use sequential_storage::cache::{Cache, Uncached};
use sequential_storage::map::{MapConfig, MapStorage};

/// Ключ настройки. `u32`, а не строка: строковый ключ пишется во flash целиком
/// при каждом сохранении, а раздел здесь — две страницы.
pub type Key = u32;

/// Ошибка хранилища. Включает и ошибки флеша, и повреждение данных в разделе.
pub type Error<E> = sequential_storage::Error<E>;

/// Рабочий буфер под сериализацию при записи.
///
/// Ограничивает максимальный размер значения: `sequential-storage` требует
/// буфер, в который поместятся ключ и значение вместе, с выравниванием по
/// слову записи флеша. 128 байт — на калибровки и настройки хватает; нужно
/// больше — правьте здесь, это не влияет ни на что, кроме RAM.
const SCRATCH: usize = 128;

/// Кеш, который ничего не кеширует (см. раздел «Чего здесь нет»).
type NoCache = Cache<Uncached, Uncached, Uncached, Key>;

/// Хранилище настроек поверх раздела флеша.
pub struct Settings<F: NorFlash> {
    map: MapStorage<Key, F, NoCache>,
    scratch: [u8; SCRATCH],
}

impl<F: NorFlash> Settings<F> {
    /// `range` — границы раздела в адресации флеша `flash` (у `embassy-stm32`
    /// нулевое смещение это база flash, а не адрес в адресном пространстве).
    ///
    /// # Паника
    ///
    /// Если раздел не годится под `sequential-storage` — не выровнен по
    /// границе страницы или меньше двух страниц. Это свойство сборки, а не
    /// данных: раскладку считает `chip-select.rhai` при генерации, поэтому
    /// падение случится на первом же запуске, а не когда-нибудь в поле.
    pub fn new(flash: F, range: Range<u32>) -> Self {
        let config = MapConfig::try_new(range.clone()).unwrap_or_else(|error| {
            panic!(
                "раздел настроек ({:#x}..{:#x}) не годится под sequential-storage: {error:?} — \
                 проверьте memory.x",
                range.start, range.end,
            )
        });
        Self {
            map: MapStorage::new(flash, config, Cache::new_uncached()),
            scratch: [0; SCRATCH],
        }
    }
}

/// Порт домена. Собственных `read`/`write` у `Settings` нет намеренно: они
/// перекрыли бы трейтовые при вызове, и приложение работало бы с конкретным
/// типом мимо порта.
impl<F: NorFlash> ports::SettingsStorage for Settings<F> {
    type Error = Error<F::Error>;

    /// Читает значение по ключу; `None` — ключ ещё не записан.
    ///
    /// Буфер отдаёт вызывающий, потому что результат ссылается прямо на него:
    /// хранилище десериализует значение без копирования.
    async fn read<'a>(
        &mut self,
        key: Key,
        scratch: &'a mut [u8],
    ) -> Result<Option<&'a [u8]>, Self::Error> {
        self.map.fetch_item::<&[u8]>(scratch, &key).await
    }

    /// Записывает значение, логически затирая предыдущее с тем же ключом.
    ///
    /// Физически идёт дозапись в конец страницы — стирание происходит, только
    /// когда страница закончилась. На этом и держится ресурс флеша, поэтому
    /// сохранять настройку в цикле опроса всё же не стоит.
    async fn write(&mut self, key: Key, value: &[u8]) -> Result<(), Self::Error> {
        self.map.store_item(&mut self.scratch, &key, &value).await
    }
}

#[cfg(test)]
mod tests {
    use ports::SettingsStorage;

    use super::Settings;
    use crate::mem_flash::{MemFlash, block_on};

    /// Две страницы по 256 байт, слово 8 — минимум, который берёт
    /// `sequential-storage`.
    type Flash = MemFlash<512, 256, 8>;

    #[test]
    fn stores_and_reads_back() {
        let mut settings = Settings::new(Flash::erased(), 0..512);
        let mut scratch = [0u8; 32];

        block_on(settings.write(7, b"template")).expect("запись");
        let back = block_on(settings.read(7, &mut scratch)).expect("чтение");

        assert_eq!(back, Some(&b"template"[..]));
    }

    #[test]
    fn a_missing_key_reads_as_none() {
        let mut settings = Settings::new(Flash::erased(), 0..512);
        let mut scratch = [0u8; 32];

        assert_eq!(
            block_on(settings.read(1, &mut scratch)).expect("чтение"),
            None
        );
    }

    #[test]
    fn a_later_write_shadows_the_earlier_one() {
        let mut settings = Settings::new(Flash::erased(), 0..512);
        let mut scratch = [0u8; 32];

        block_on(settings.write(3, b"first")).expect("первая");
        block_on(settings.write(3, b"second")).expect("вторая");

        assert_eq!(
            block_on(settings.read(3, &mut scratch)).expect("чтение"),
            Some(&b"second"[..])
        );
    }

    #[test]
    #[should_panic(expected = "не годится под sequential-storage")]
    fn refuses_a_partition_smaller_than_two_pages() {
        let _ = Settings::new(Flash::erased(), 0..256);
    }
}
