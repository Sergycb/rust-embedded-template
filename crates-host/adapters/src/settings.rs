// {%- if config != "true" %}
//! Заглушка хранилища настроек для узла CANopen, у которого раздела настроек нет.
// {%- endif %}
// {%- if config == "true" %}
//! Настройки во flash: пары «ключ — значение», переживающие и перезапуск, и
//! обновление прошивки, — поверх `sequential-storage`.
//!
//! Раздел под них режет генерация (`CONFIG`, две страницы стирания — меньше
//! `sequential-storage` не берёт; в хвосте flash или, с OTA на F2/F4/F7, в
//! мелких секторах перед `ACTIVE`), а приносит его `bsp::flash` готовым
//! `NorFlash`{% if erase_page == "true" %} (страница — `flash_page::ErasePage` вокруг флеша){% endif %}, и хранилище берёт весь его объём; самому хранилищу всё равно, что за флеш под ним, — оттого оно
//! здесь и тестируется на хосте с флешем в памяти. Чем сериализовать значение
//! (postcard, `miniconf`, свой формат), решает проект.
//!
//! ```ignore
//! // Чтение и запись живут в порту домена — без импорта методов не видно.
//! use ports::SettingsStorage;
//!
//! // Хранилище — на весь носитель платы (`board.config`); строит его
//! // compose-site и отдаёт узлу во входе.
//! let mut settings = Settings::whole(board.config);
//! let mut scratch = [0u8; 64];
//! let mut buf = [0u8; 64];
//! settings.write(KEY_CALIBRATION, &postcard::to_slice(&cal, &mut buf)?).await?;
//! if let Some(raw) = settings.read(KEY_CALIBRATION, &mut scratch).await? {
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
//!     if let Some(raw) = settings.read(key, &mut scratch).await? {
//!         miniconf::postcard::set_by_key(&mut config, &path, postcard::de_flavors::Slice::new(raw))?;
//!     }
//! }
//!
//! // Изменился один лист — пишется один лист.
//! let path = Path::<String<32>, '/'>::from("/gain/1");
//! let mut buf = [0u8; 64];
//! let raw = miniconf::postcard::get_by_key(&config, &path, postcard::ser_flavors::Slice::new(&mut buf))?;
//! settings.write(yafnv::fnv1a(path.as_str().as_bytes()), raw).await?;
//! ```
//!
//! # Смена формата значения
//!
//! Образ, поменявший формат значения, пишет его под новым ключом, а старую
//! запись не трогает. Неразборчивое значение — `Default` и строка в лог, а не
//! отказ узла: узлы не падают.
// {%- if ota == "true" %}
//!
//! Причина — откат: образ, не дошедший до `mark_booted`, bootloader заменит
//! прежним, и тот прочитает свой ключ в своём формате. Перепиши новый образ
//! запись на месте — прежнему досталось бы значение в чужом формате, а
//! `sequential-storage` старого не хранит.
// {%- endif %}
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
// {%- endif %}

// {%- if config == "true" %}
use core::ops::Range;

use embedded_storage_async::nor_flash::NorFlash;
use sequential_storage::cache::{Cache, Uncached};
use sequential_storage::map::{MapConfig, MapStorage};

/// Рабочий буфер: в нём запись сериализуется и в нём же ищется при чтении.
///
/// Ограничивает максимальный размер значения: `sequential-storage` требует
/// буфер, в который поместятся ключ и значение вместе, с выравниванием по
/// слову записи флеша. 128 байт — на калибровки и настройки хватает; нужно
/// больше — правьте здесь, это не влияет ни на что, кроме RAM.
///
/// Поиск читает в этот буфер каждую запись раздела, поэтому уменьшать его
/// нельзя, пока в разделе лежат записи прошивки с большим буфером: на первой
/// такой записи любое чтение откажет с `BufferTooSmall`.
const SCRATCH: usize = 128;
// {%- endif %}

/// Ключ настройки. `u32`, а не строка: строковый ключ пишется во flash целиком
/// при каждом сохранении, а раздел здесь — две страницы.
pub type Key = u32;

// {%- if config == "true" %}
/// Ошибка хранилища. Включает и ошибки флеша, и повреждение данных в разделе.
pub type Error<E> = sequential_storage::Error<E>;

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

// {%- endif %}

// {%- if config == "true" %}
impl<F: NorFlash> Settings<F> {
    // {%- if config_store == "eeprom" %}
    /// Хранилище на весь носитель платы (внутренняя EEPROM, `board.config`).
    // {%- elsif config_store == "external" %}
    /// Хранилище на весь носитель платы (раздел внешней флеши, `board.config`).
    // {%- else %}
    /// Хранилище на весь носитель платы (раздел флеша, `board.config`).
    // {%- endif %}
    ///
    /// # Panics
    ///
    /// Как [`Settings::new`]: носитель не годится под `sequential-storage`.
    pub fn whole(flash: F) -> Self {
        let end = u32::try_from(flash.capacity()).expect("носитель настроек больше 4 GiB");
        Self::new(flash, 0..end)
    }
}
// {%- endif %}

// {%- if has_canopen == "true" and config != "true" %}
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

    /// Предела нет: ничего не хранится.
    const MAX_VALUE: usize = usize::MAX;

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
// {%- endif %}

// {%- if config == "true" %}
/// Порт домена. Собственных `read`/`write` у `Settings` нет намеренно: они
/// перекрыли бы трейтовые при вызове, и приложение работало бы с конкретным
/// типом мимо порта.
impl<F: NorFlash> ports::SettingsStorage for Settings<F> {
    type Error = Error<F::Error>;

    /// Ключ и значение, дополненные до слова флеша, обязаны поместиться в
    /// рабочий буфер (128 байт): столько слов, сколько в нём целиком, без ключа.
    const MAX_VALUE: usize = SCRATCH / F::WRITE_SIZE * F::WRITE_SIZE - size_of::<Key>();

    /// Читает значение по ключу; `None` — ключ ещё не записан.
    ///
    /// Ищет во внутреннем буфере, а не в буфере вызывающего: `sequential-storage`
    /// читает в буфер поиска каждую запись раздела и на любой, что длиннее, —
    /// даже с чужим ключом — отказывает с `BufferTooSmall`. Внутренний вмещает
    /// любую запись этого хранилища, вызывающему копируется только значение;
    /// не влезло — `BufferTooSmall(длина значения)`.
    async fn read<'a>(
        &mut self,
        key: Key,
        scratch: &'a mut [u8],
    ) -> Result<Option<&'a [u8]>, Self::Error> {
        let Some(value) = self
            .map
            .fetch_item::<&[u8]>(&mut self.scratch, &key)
            .await?
        else {
            return Ok(None);
        };
        let out = scratch
            .get_mut(..value.len())
            .ok_or(sequential_storage::Error::BufferTooSmall(value.len()))?;
        out.copy_from_slice(value);
        Ok(Some(out))
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

    #[test]
    #[should_panic(expected = "не годится под sequential-storage")]
    fn refuses_a_partition_off_the_page_boundary() {
        let _ = Settings::new(Flash::erased(), 8..520);
    }

    /// Значение, которое вместе с ключом не лезет в рабочий буфер, — ошибка
    /// записи, а не паника и не обрезанная запись.
    #[tokio::test]
    async fn a_value_bigger_than_the_scratch_is_an_error() {
        let mut settings = Settings::new(Flash::erased(), 0..512);
        let mut scratch = [0u8; 256];

        let refused = settings.write(1, &[0xAB; super::SCRATCH]).await;

        assert!(refused.is_err(), "{refused:?}");
        assert_eq!(settings.read(1, &mut scratch).await.expect("чтение"), None);
    }

    /// Буфера вызывающего хватает по размеру искомого значения: запись с чужим
    /// ключом длиннее буфера чтению не мешает.
    #[tokio::test]
    async fn a_short_value_reads_past_a_longer_record() {
        let mut settings = Settings::new(Flash::erased(), 0..512);
        let mut scratch = [0u8; 8];

        settings.write(1, b"ok").await.expect("короткая");
        settings.write(2, &[0xAB; 100]).await.expect("длинная");

        assert_eq!(
            settings.read(1, &mut scratch).await.expect("чтение"),
            Some(&b"ok"[..])
        );
    }

    /// Значение длиннее буфера вызывающего — ошибка с длиной значения, а не
    /// обрезанное значение.
    #[tokio::test]
    async fn a_value_longer_than_the_buffer_is_an_error() {
        let mut settings = Settings::new(Flash::erased(), 0..512);
        let mut scratch = [0u8; 8];

        settings.write(1, &[0xAB; 20]).await.expect("запись");
        let refused = settings.read(1, &mut scratch).await;

        assert!(
            matches!(refused, Err(sequential_storage::Error::BufferTooSmall(20))),
            "{refused:?}"
        );
    }

    /// Раздел переживает перезапуск: новое хранилище над тем же флешем видит
    /// записанное прежним.
    #[tokio::test]
    async fn a_new_storage_over_the_same_flash_reads_what_was_written() {
        let mut first = Settings::new(Flash::erased(), 0..512);
        first.write(5, b"kept").await.expect("запись");
        first.write(5, b"newest").await.expect("перезапись");
        let mem = core::mem::take(&mut first.map.flash().mem);

        let mut second = Settings::new(Flash { mem }, 0..512);
        let mut scratch = [0u8; 32];

        assert_eq!(
            second.read(5, &mut scratch).await.expect("чтение"),
            Some(&b"newest"[..])
        );
    }

    // {%- if has_canopen == "true" and config != "true" %}
    /// `Unavailable` читается пустым, а запись «успешна» и ничего не хранит —
    /// как заявлено в документации.
    #[tokio::test]
    async fn unavailable_reads_empty_and_forgets_writes() {
        let mut settings = super::Unavailable;
        let mut scratch = [0u8; 8];

        settings.write(1, b"lost").await.expect("запись «успешна»");

        assert_eq!(settings.read(1, &mut scratch).await.expect("чтение"), None);
    }
    // {%- endif %}
}
// {%- endif %}
