//! Настройки во flash: пары «ключ — значение», переживающие и перезапуск, и
//! обновление прошивки, — поверх `sequential-storage`.
// {%- if config != "true" %}
//!
//! Раздела настроек у проекта нет: тип нужен узлу CANopen, которому
//! хранилище не обязательно, — он получает `None` ([`NoFlash`](crate::settings::NoFlash)).
// {%- endif %}
// {%- if config == "true" %}
//!
//! Раздел под них режет генерация (`CONFIG`, две страницы стирания — меньше
//! `sequential-storage` не берёт; в хвосте flash или, с OTA на F2/F4/F7, в
//! мелких секторах перед `ACTIVE`), а приносит его `bsp::flash` готовым
//! `NorFlash`{% if erase_page == "true" %} (страница — `flash_page::ErasePage` вокруг флеша){% endif %}, и хранилище берёт весь его объём; самому хранилищу всё равно, что за флеш под ним, — оттого оно
//! здесь и тестируется на хосте с флешем в памяти. Чем сериализовать значение
//! (postcard, `miniconf`, свой формат), решает проект.
//!
//! ```ignore
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
//! # Настройки как дерево: `miniconf` поверх хранилища
//!
//! Структура целиком под одним ключом — самый короткий путь, но с первым же
//! добавленным полем старая запись перестаёт читаться. Приём из экосистемы
//! Stabilizer (`serial-settings`): `#[derive(Tree)]` на структуре настроек, и
//! каждый лист адресуется путём (`/gain/1`); во flash уезжает по листу под
//! ключом `fnv1a(путь)` — ровно `u32`, которого ждёт хранилище. Новое поле
//! остаётся `Default`, старые читаются. Сам `miniconf` ничего не хранит и
//! зависимостью шаблона не является — это выбор проекта, как и `postcard` с
//! `yafnv` в этом наброске (имена функций — по документации версии, которую
//! возьмёте).
//!
//! ```ignore
//! use heapless::String;
//! use miniconf::{Path, Tree, TreeSchema};
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
//! # Повреждённый раздел
//!
//! Обрыв питания посреди стирания или переноса записей `sequential-storage`
//! чинит сам: на `Corrupted` любая его операция зовёт `try_repair` и
//! повторяет себя; одну битую страницу из двух он тоже разбирает сам, хоть и
//! не всегда без потери записей. Не починилось — раздел стирается целиком, и
//! операция идёт уже над пустым: настройки потеряны, а не заперты навсегда, и
//! узлы читают «ключа нет» — то же, что на новой плате. В лог прошивки — одна
//! строка о стирании.
//! `Corrupted` наружу не уходит; не удалось само стирание — наружу ошибка
//! флеша, и следующая операция попробует снова.
//!
//! # Чего здесь нет
//!
//! **Удаления ключа.** `sequential_storage::map::remove_item` требует
//! [`MultiwriteNorFlash`](embedded_storage_async::nor_flash::MultiwriteNorFlash) —
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

use core::ops::Range;

use defmt_or_log::warn;
use embedded_storage_async::nor_flash::NorFlash;
// {%- if has_canopen == "true" and config != "true" %}
use embedded_storage_async::nor_flash::{ErrorType, ReadNorFlash};
// {%- endif %}
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

/// Ключ настройки. `u32`, а не строка: строковый ключ пишется во flash целиком
/// при каждом сохранении, а раздел здесь — две страницы.
pub type Key = u32;

/// Ошибка хранилища — ошибка `sequential-storage` над ошибкой флеша.
/// `Corrupted` [`Settings`] наружу не отдаёт — повреждённый раздел стирается
/// (см. «Повреждённый раздел» в документации модуля).
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

    /// Носитель — тестам, которые смотрят во флеш под хранилищем.
    #[cfg(test)]
    pub(crate) fn flash(&mut self) -> &mut F {
        self.map.flash()
    }

    /// Раздел повреждён, и `sequential-storage` его не починил: стирает
    /// целиком — настройки потеряны, но хранилище снова работает.
    ///
    /// `Corrupted` до сюда доходит уже после попытки самого крейта: на нём
    /// любая операция зовёт свой `try_repair` (дотирает недостёртые страницы,
    /// повторяет прерванный перенос записей) и повторяет себя. Не помогло —
    /// чинить больше нечем, и крейт сам советует стереть раздел.
    async fn erase_corrupted(&mut self) -> Result<(), Error<F::Error>> {
        warn!("settings: раздел повреждён и не починился — стёрт, настройки сброшены");
        self.map.erase_all().await
    }
}

impl<F: NorFlash> Settings<F> {
    /// Самое длинное значение, которое хранилище запишет и прочитает, байт.
    ///
    /// Ключ и значение, дополненные до слова флеша, обязаны поместиться в
    /// рабочий буфер (128 байт): столько слов, сколько в нём целиком, без
    /// ключа. Предел считается от настоящего слова носителя, а не от худшего
    /// среди чипов; узел, который хранит значения известной длины, сверяет её
    /// с пределом `const`-проверкой.
    pub const MAX_VALUE: usize = SCRATCH / F::WRITE_SIZE * F::WRITE_SIZE - size_of::<Key>();

    /// Читает значение по ключу; `None` — ключ ещё не записан.
    ///
    /// Значение копируется в буфер вызывающего, и результат ссылается на него.
    /// Ищет во внутреннем буфере, а не в буфере вызывающего: `sequential-storage`
    /// читает в буфер поиска каждую запись раздела и на любой, что длиннее, —
    /// даже с чужим ключом — отказывает с `BufferTooSmall`. Внутренний вмещает
    /// любую запись этого хранилища, вызывающему копируется только значение;
    /// не влезло — `BufferTooSmall(длина значения)`.
    ///
    /// Раздел повреждён — стирается (см. «Повреждённый раздел»), и ключ
    /// читается как ещё не записанный.
    ///
    /// # Errors
    ///
    /// Ошибка флеша, если чтение не удалось, или `BufferTooSmall`, если
    /// значение длиннее буфера, — обрезанного значения не бывает.
    pub async fn read<'a>(
        &mut self,
        key: Key,
        scratch: &'a mut [u8],
    ) -> Result<Option<&'a [u8]>, Error<F::Error>> {
        let found = match self.map.fetch_item::<&[u8]>(&mut self.scratch, &key).await {
            Err(sequential_storage::Error::Corrupted { .. }) => {
                self.erase_corrupted().await?;
                None
            }
            found => found?,
        };
        let Some(value) = found else {
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
    ///
    /// Раздел повреждён — стирается (см. «Повреждённый раздел»), и значение
    /// пишется в пустой.
    ///
    /// # Errors
    ///
    /// Ошибка флеша, если запись не удалась, или ошибка `sequential-storage`,
    /// если значение длиннее [`MAX_VALUE`](Self::MAX_VALUE).
    pub async fn write(&mut self, key: Key, value: &[u8]) -> Result<(), Error<F::Error>> {
        match self.map.store_item(&mut self.scratch, &key, &value).await {
            Err(sequential_storage::Error::Corrupted { .. }) => {
                self.erase_corrupted().await?;
                self.map.store_item(&mut self.scratch, &key, &value).await
            }
            done => done,
        }
    }
}
// {%- if has_canopen == "true" and config != "true" %}

/// Носителя нет: у проекта нет раздела настроек.
///
/// Значений у типа нет, и `Option<Settings<NoFlash>>` — всегда `None`: так
/// узел, которому хранилище не обязательно (`domain::canopen`), получает
/// «хранилища нет» без своего типа-заглушки. Флешем тип притворяется только
/// ради границы `Settings<F: NorFlash>`: вызвать его методы не на чем.
#[derive(Debug)]
pub enum NoFlash {}

impl ErrorType for NoFlash {
    type Error = core::convert::Infallible;
}

impl ReadNorFlash for NoFlash {
    const READ_SIZE: usize = 1;

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "значений у типа нет: тело не исполняется никогда"
    )]
    async fn read(&mut self, _: u32, _: &mut [u8]) -> Result<(), Self::Error> {
        match *self {}
    }

    fn capacity(&self) -> usize {
        match *self {}
    }
}

impl NorFlash for NoFlash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = 1;

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "значений у типа нет: тело не исполняется никогда"
    )]
    async fn erase(&mut self, _: u32, _: u32) -> Result<(), Self::Error> {
        match *self {}
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "значений у типа нет: тело не исполняется никогда"
    )]
    async fn write(&mut self, _: u32, _: &[u8]) -> Result<(), Self::Error> {
        match *self {}
    }
}
// {%- endif %}

#[cfg(test)]
mod tests {
    use adapters_hw::fake::MemFlash;

    use super::Settings;

    /// Флеш под `sequential-storage`: две страницы по 256 байт (меньше он не
    /// берёт), слово 8.
    type Flash = MemFlash<512, 8, 256>;

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
        let mem = core::mem::take(&mut first.flash().mem);

        let mut second = Settings::new(Flash { mem }, 0..512);
        let mut scratch = [0u8; 32];

        assert_eq!(
            second.read(5, &mut scratch).await.expect("чтение"),
            Some(&b"newest"[..])
        );
    }

    /// Флеш, в котором записаны ключи 1 и 2, а страница `page` затёрта
    /// мусором `0x5A`.
    async fn with_a_broken_page(page: usize) -> Settings<Flash> {
        let mut settings = Settings::new(Flash::erased(), 0..512);
        settings.write(1, b"one").await.expect("запись 1");
        settings.write(2, b"two").await.expect("запись 2");
        let mut mem = core::mem::take(&mut settings.flash().mem);
        mem[page * 256..][..256].fill(0x5A);
        Settings::new(Flash { mem }, 0..512)
    }

    /// Раздел целиком в мусоре: `sequential-storage` отвечает `Corrupted` и
    /// после своего ремонта, и хранилище стирает раздел — чтение даёт «ключа
    /// нет», запись и чтение дальше работают.
    #[tokio::test]
    async fn a_garbage_partition_is_erased_and_works_again() {
        let mut settings = Settings::new(Flash::filled(0x5A), 0..512);
        let mut scratch = [0u8; 32];

        assert_eq!(settings.read(1, &mut scratch).await.expect("чтение"), None);
        settings.write(1, b"fresh").await.expect("запись");
        assert_eq!(
            settings.read(1, &mut scratch).await.expect("чтение"),
            Some(&b"fresh"[..])
        );
    }

    /// Записи были, но обе страницы помечены закрытыми — такого раздела
    /// `sequential-storage` не чинит. Первой операцией оказалась запись: она
    /// стирает раздел и пишет в пустой, а прежние ключи читаются как
    /// отсутствующие.
    #[tokio::test]
    async fn a_write_to_an_unrepairable_partition_erases_it_first() {
        let mut settings = Settings::new(Flash::erased(), 0..512);
        settings.write(1, b"one").await.expect("запись 1");
        let mut mem = core::mem::take(&mut settings.flash().mem);
        // Маркеры состояния страницы — её первый и последний байт.
        for marker in [0, 255, 256, 511] {
            mem[marker] = 0;
        }
        let mut settings = Settings::new(Flash { mem }, 0..512);
        let mut scratch = [0u8; 32];

        settings.write(2, b"fresh").await.expect("запись");

        assert_eq!(
            settings.read(2, &mut scratch).await.expect("чтение"),
            Some(&b"fresh"[..])
        );
        assert_eq!(settings.read(1, &mut scratch).await.expect("чтение"), None);
    }

    /// Одна битая страница из двух: исход определён и без стирания всего
    /// раздела — его даёт ремонт самого `sequential-storage`. Битая страница с
    /// записями теряет свои ключи. Битая вторая страница читается закрытой, и
    /// записи целой видны, пока первая запись не запустит ремонт: тот считает
    /// перенос прерванным и повторяет его с битой страницы — записи пропадают и
    /// здесь. В обоих случаях запись и чтение дальше работают.
    #[tokio::test]
    async fn one_broken_page_of_two_has_a_defined_outcome() {
        for (page, before) in [(0, None), (1, Some(&b"one"[..]))] {
            let mut settings = with_a_broken_page(page).await;
            let mut scratch = [0u8; 32];

            assert_eq!(
                settings.read(1, &mut scratch).await.expect("чтение"),
                before,
                "страница {page}"
            );
            settings.write(3, b"three").await.expect("запись");
            assert_eq!(
                settings.read(3, &mut scratch).await.expect("чтение"),
                Some(&b"three"[..]),
                "страница {page}"
            );
        }
    }
    // {%- if config_store == "eeprom" %}

    /// Главное для EEPROM L0/L1: хранилище работает поверх неё с завода, без
    /// первичной подготовки, — заводские нули обёртка
    /// (`adapters_hw::eeprom::Eeprom`) читает стёртым флешем.
    #[tokio::test]
    async fn settings_work_on_a_fresh_eeprom() {
        use core::cell::RefCell;

        use adapters_hw::eeprom::Eeprom;
        use embedded_storage::{ReadStorage, Storage};

        /// EEPROM в памяти: с завода нули, как у L0/L1.
        struct MemEeprom(RefCell<Vec<u8>>);

        impl ReadStorage for &MemEeprom {
            type Error = ();

            fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), ()> {
                let start = offset as usize;
                buf.copy_from_slice(&self.0.borrow()[start..start + buf.len()]);
                Ok(())
            }

            fn capacity(&self) -> usize {
                self.0.borrow().len()
            }
        }

        impl Storage for &MemEeprom {
            fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), ()> {
                let start = offset as usize;
                self.0.borrow_mut()[start..start + data.len()].copy_from_slice(data);
                Ok(())
            }
        }

        let eeprom = MemEeprom(RefCell::new(vec![0; 512]));
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
    // {%- endif %}
}
