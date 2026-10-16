//! Настройки во flash: пары «ключ — значение», переживающие и перезапуск, и
//! обновление прошивки, — поверх `sequential-storage`.
//!
//! Раздел под них режет генерация (`CONFIG`, две страницы стирания — меньше
//! `sequential-storage` не берёт; в хвосте flash или, с OTA на F2/F4/F7, в
//! мелких секторах перед `ACTIVE`), а границы приносит `bsp::config`
//! диапазоном, страницу — `adapters::flash::Paged` вокруг флеша; самому хранилищу всё равно, что за флеш под ним, — оттого оно
//! здесь и тестируется на хосте с флешем в памяти. Чем сериализовать значение
//! (postcard, `miniconf`, свой формат), решает проект.
//!
//! ```ignore
//! // Чтение и запись живут в порту домена — без импорта методов не видно.
//! use ports::SettingsStorage;
//!
//! let mut scratch = [0u8; 64];
//! let mut buf = [0u8; 64];
//! board.settings.write(KEY_CALIBRATION, &postcard::to_slice(&cal, &mut buf)?).await?;
//! if let Some(raw) = board.settings.read(KEY_CALIBRATION, &mut scratch).await? {
//!     let cal: Calibration = postcard::from_bytes(raw)?;
//! }
//! ```
//!
//! # Настройки как дерево: `miniconf` поверх порта
//!
//! Структура целиком под одним ключом — самый короткий путь, но с первым же
//! добавленным полем старая запись перестаёт читаться. Приём из экосистемы
//! Stabilizer (`serial-settings`): `#[derive(Tree)]` на структуре настроек, и
//! каждый лист адресуется путём (`/gain/1`); во flash уезжает по листу под
//! ключом `fnv1a(путь)` — ровно `u32`, которого ждёт порт. Новое поле
//! остаётся `Default`, старые читаются. Сам `miniconf` ничего не хранит и
//! зависимостью шаблона не является — это выбор проекта, как и `postcard` с
//! `yafnv` в этом наброске (имена функций — по документации версии, которую
//! возьмёте).
//!
//! ```ignore
//! use heapless::String;
//! use miniconf::{Path, Tree, TreeSchema};
//! use ports::SettingsStorage;
//!
//! #[derive(Default, Tree)]
//! struct Config {
//!     gain: [u16; 2],
//!     enabled: bool,
//! }
//!
//! // Старт: пройти все листья, прочитать те, что уже записаны.
//! let mut config = Config::default();
//! let mut scratch = [0u8; 64];
//! for node in Config::nodes::<Path<String<32>, '/'>, 2>() {
//!     let (path, _node) = node?;
//!     let key: u32 = yafnv::fnv1a(path.as_str().as_bytes());
//!     if let Some(raw) = board.settings.read(key, &mut scratch).await? {
//!         miniconf::postcard::set_by_key(&mut config, &path, postcard::de_flavors::Slice::new(raw))?;
//!     }
//! }
//!
//! // Изменился один лист — пишется один лист.
//! let path = Path::<String<32>, '/'>::from("/gain/1");
//! let mut buf = [0u8; 64];
//! let raw = miniconf::postcard::get_by_key(&config, &path, postcard::ser_flavors::Slice::new(&mut buf))?;
//! board.settings.write(yafnv::fnv1a(path.as_str().as_bytes()), raw).await?;
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

impl<F: NorFlash> core::fmt::Debug for Settings<F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Settings").finish_non_exhaustive()
    }
}

impl<F: NorFlash> Settings<F> {
    /// `range` — границы раздела в адресации флеша `flash` (у `embassy-stm32`
    /// нулевое смещение это база flash, а не адрес в адресном пространстве).
    ///
    /// # Panics
    ///
    /// Паникует, если раздел не годится под `sequential-storage` — не выровнен
    /// по границе страницы или меньше двух страниц. Это свойство сборки, а не
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

/// Хранилища нет — у проекта нет раздела настроек.
///
/// Для узлов, которым хранилище нужно по сигнатуре, но не обязательно для
/// работы (`domain::canopen` без раздела настроек сохранять не берётся):
/// читается пустым, а запись отвечает успехом и данные не хранит — узлу,
/// который всё же в неё пишет, она не годится.
#[derive(Debug, Default)]
pub struct Unavailable;

impl ports::SettingsStorage for Unavailable {
    type Error = core::convert::Infallible;

    fn read<'a>(
        &mut self,
        _: Key,
        _: &'a mut [u8],
    ) -> impl Future<Output = Result<Option<&'a [u8]>, Self::Error>> {
        core::future::ready(Ok(None))
    }

    fn write(&mut self, _: Key, _: &[u8]) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Ok(()))
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
    use embedded_storage::nor_flash::{ErrorType, NorFlashErrorKind};
    use embedded_storage_async::nor_flash::{NorFlash, ReadNorFlash};
    use ports::SettingsStorage;

    use super::Settings;

    /// Асинхронный флеш в памяти — то, чего ждёт `sequential-storage`: две
    /// страницы по 256 байт (меньше он не берёт), слово 8, стирание в `0xFF`,
    /// запись только «единицы в нули» — иначе не проверить, что хранилище не
    /// пишет поверх нестёртого.
    #[derive(Debug)]
    struct Flash {
        mem: Vec<u8>,
    }

    impl Flash {
        fn erased() -> Self {
            Self {
                mem: vec![0xFF; 512],
            }
        }
    }

    impl ErrorType for Flash {
        type Error = NorFlashErrorKind;
    }

    impl ReadNorFlash for Flash {
        const READ_SIZE: usize = 1;

        fn read(
            &mut self,
            offset: u32,
            bytes: &mut [u8],
        ) -> impl Future<Output = Result<(), Self::Error>> {
            let start = offset as usize;
            bytes.copy_from_slice(&self.mem[start..start + bytes.len()]);
            core::future::ready(Ok(()))
        }

        fn capacity(&self) -> usize {
            self.mem.len()
        }
    }

    impl NorFlash for Flash {
        const WRITE_SIZE: usize = 8;
        const ERASE_SIZE: usize = 256;

        fn erase(&mut self, from: u32, to: u32) -> impl Future<Output = Result<(), Self::Error>> {
            self.mem[from as usize..to as usize].fill(0xFF);
            core::future::ready(Ok(()))
        }

        fn write(
            &mut self,
            offset: u32,
            bytes: &[u8],
        ) -> impl Future<Output = Result<(), Self::Error>> {
            for (cell, byte) in self.mem[offset as usize..].iter_mut().zip(bytes) {
                *cell &= *byte;
            }
            core::future::ready(Ok(()))
        }
    }

    #[tokio::test]
    async fn stores_and_reads_back() {
        let mut settings = Settings::new(Flash::erased(), 0..512);
        let mut scratch = [0u8; 32];

        settings.write(7, b"template").await.expect("запись");
        let back = settings.read(7, &mut scratch).await.expect("чтение");

        assert_eq!(back, Some(&b"template"[..]));
    }

    #[tokio::test]
    async fn a_missing_key_reads_as_none() {
        let mut settings = Settings::new(Flash::erased(), 0..512);
        let mut scratch = [0u8; 32];

        assert_eq!(settings.read(1, &mut scratch).await.expect("чтение"), None);
    }

    #[tokio::test]
    async fn a_later_write_shadows_the_earlier_one() {
        let mut settings = Settings::new(Flash::erased(), 0..512);
        let mut scratch = [0u8; 32];

        settings.write(3, b"first").await.expect("первая");
        settings.write(3, b"second").await.expect("вторая");

        assert_eq!(
            settings.read(3, &mut scratch).await.expect("чтение"),
            Some(&b"second"[..])
        );
    }

    #[test]
    #[should_panic(expected = "не годится под sequential-storage")]
    fn refuses_a_partition_smaller_than_two_pages() {
        let _ = Settings::new(Flash::erased(), 0..256);
    }
}
