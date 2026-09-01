//! Настройки во flash: пары «ключ — значение», переживающие и перезапуск, и
//! обновление прошивки.
//!
//! Раздел `CONFIG` отрезан от хвоста flash при генерации (ровно две последние
//! страницы стирания — меньше `sequential-storage` не берёт), а его границы
//! приезжают сюда символами линкера из `memory.x`. Спрашивается он при
//! генерации отдельным вопросом: страница здесь — это `NorFlash::ERASE_SIZE`
//! цельного `Flash`, то есть МАКСИМАЛЬНЫЙ сектор чипа, и на F4/F7/H7 раздел
//! стоит 256 KiB.
//!
//! Зачем это в шаблоне. Дереву настроек («адресуемое дерево, доступ к листьям
//! по JSON-пути», например `miniconf`) раньше было некуда сохраняться: в
//! `memory.x` не было ни одного сектора под данные, и любая калибровка жила
//! до первого сброса. Здесь — только хранилище; чем сериализовать значение
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

use embassy_stm32::flash::{Blocking, Flash};
use sequential_storage::cache::{Cache, Uncached};
use sequential_storage::map::{MapConfig, MapStorage};

use crate::FlashMutex;

/// Ключ настройки. `u32`, а не строка: строковый ключ пишется во flash целиком
/// при каждом сохранении, а раздел здесь — две страницы.
pub type Key = u32;

/// Ошибка хранилища. Включает и ошибки флеша, и повреждение данных в разделе.
pub type Error = sequential_storage::Error<embassy_stm32::flash::Error>;

/// Рабочий буфер под сериализацию при записи.
///
/// Ограничивает максимальный размер значения: `sequential-storage` требует
/// буфер, в который поместятся ключ и значение вместе, с выравниванием по
/// слову записи флеша. 128 байт — на калибровки и настройки хватает; нужно
/// больше — правьте здесь, это не влияет ни на что, кроме RAM.
const SCRATCH: usize = 128;

/// Тип флеша, поверх которого работает хранилище.
type Inner = Flash<'static, Blocking>;

/// Кеш, который ничего не кеширует (см. раздел «Чего здесь нет»).
type NoCache = Cache<Uncached, Uncached, Uncached, Key>;

unsafe extern "C" {
    /// Границы раздела `CONFIG` из `memory.x`, отсчитанные от базы flash —
    /// именно так их ждёт `embassy_stm32::flash::Flash` (у него нулевое
    /// смещение это база, а не адрес в адресном пространстве).
    static __config_start: u32;
    static __config_end: u32;
}

/// Хранилище настроек поверх раздела `CONFIG`.
pub struct Settings {
    map: MapStorage<Key, SharedFlash, NoCache>,
    scratch: [u8; SCRATCH],
}

impl Settings {
    /// # Паника
    ///
    /// Если раздел `CONFIG` не годится под `sequential-storage` — не выровнен
    /// по границе страницы или меньше двух страниц. Это свойство сборки, а не
    /// данных: раскладку считает `chip-select.rhai` при генерации, поэтому
    /// падение случится на первом же запуске, а не когда-нибудь в поле.
    pub fn new(flash: &'static FlashMutex) -> Self {
        let range = flash_range();
        let config = MapConfig::try_new(range.clone()).unwrap_or_else(|error| {
            panic!(
                "раздел CONFIG ({:#x}..{:#x}) не годится под настройки: {error:?} — проверьте \
                 memory.x",
                range.start, range.end,
            )
        });
        Self {
            map: MapStorage::new(SharedFlash { flash }, config, Cache::new_uncached()),
            scratch: [0; SCRATCH],
        }
    }
}

/// Порт домена поверх `sequential-storage`.
///
/// Обе операции были собственными методами `Settings` и переехали сюда
/// целиком, а не продублированы: собственный метод с тем же именем перекрыл
/// бы трейтовый при вызове, и приложение работало бы с конкретным типом `bsp`
/// вместо порта.
impl ports::SettingsStorage for Settings {
    type Error = Error;

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
    ///
    /// Буфер здесь свой, в отличие от чтения: наружу ничего не возвращается,
    /// а значит и заимствовать нечего.
    async fn write(&mut self, key: Key, value: &[u8]) -> Result<(), Self::Error> {
        self.map.store_item(&mut self.scratch, &key, &value).await
    }
}

/// Границы раздела по символам линкера.
fn flash_range() -> Range<u32> {
    // SAFETY: символы объявлены линкерным скриптом как абсолютные адреса;
    // читается их адрес, а не содержимое (памяти за ними нет).
    let start = &raw const __config_start as u32;
    let end = &raw const __config_end as u32;
    start..end
}

/// Асинхронные трейты флеша поверх общего `Flash`, живущего под
/// `blocking_mutex`.
///
/// Нужна, потому что API `sequential-storage` 8.x асинхронный, а `Flash`
/// embassy — блокирующий и делится с OTA. Взять `&mut Flash` из мьютекса и
/// держать его через `.await` нельзя (`lock` даёт доступ только внутри
/// замыкания), поэтому блокировка берётся на каждую операцию отдельно. Это
/// честно: сами операции блокирующие, ни одна из них внутри не ждёт, и
/// «асинхронность» здесь чистая формальность ради совместимости типов.
struct SharedFlash {
    flash: &'static FlashMutex,
}

impl embedded_storage::nor_flash::ErrorType for SharedFlash {
    type Error = embassy_stm32::flash::Error;
}

impl embedded_storage_async::nor_flash::ReadNorFlash for SharedFlash {
    const READ_SIZE: usize = <Inner as embedded_storage::nor_flash::ReadNorFlash>::READ_SIZE;

    async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.flash.lock(|flash| {
            embedded_storage::nor_flash::ReadNorFlash::read(&mut *flash.borrow_mut(), offset, bytes)
        })
    }

    fn capacity(&self) -> usize {
        self.flash
            .lock(|flash| embedded_storage::nor_flash::ReadNorFlash::capacity(&*flash.borrow()))
    }
}

impl embedded_storage_async::nor_flash::NorFlash for SharedFlash {
    const WRITE_SIZE: usize = <Inner as embedded_storage::nor_flash::NorFlash>::WRITE_SIZE;
    const ERASE_SIZE: usize = <Inner as embedded_storage::nor_flash::NorFlash>::ERASE_SIZE;

    async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.flash.lock(|flash| {
            embedded_storage::nor_flash::NorFlash::erase(&mut *flash.borrow_mut(), from, to)
        })
    }

    async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.flash.lock(|flash| {
            embedded_storage::nor_flash::NorFlash::write(&mut *flash.borrow_mut(), offset, bytes)
        })
    }
}
