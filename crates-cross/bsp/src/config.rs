//! Обвязка хранилища настроек этой платы: на чём оно лежит и псевдоним типа
//! для задач.
//!
//! Само хранилище — `adapters::settings::Settings` (generic по флешу,
//! тестируется на хосте); ему годится любой асинхронный `NorFlash`. Здесь
//! остаётся то, что знает про плату: что именно в этот слот подставлено. При
//! генерации выбрано одно из трёх (`config` в cargo-generate, см.
//! docs/flash.md):
//!
//! - раздел во внутреннем флеше — границы `CONFIG` приезжают символами
//!   `memory.x`, страница — [`CONFIG_ERASE_SIZE`];
//! - внутренняя EEPROM (STM32L0/L1) — через `adapters::eeprom::Eeprom`;
//! - внешняя флеш — ваш драйвер; пока на его месте заглушка
//!   `adapters::flash::Unconnected`.
{%- if config_store == "flash" %}
//!
//! Здесь — раздел во внутреннем флеше: две страницы стирания (меньше
//! `sequential-storage` не берёт), положенные при генерации туда, где они
//! дешевле: с OTA на F2/F4/F7 — в мелкие сектора перед `ACTIVE` (у F4 —
//! 2×16 KiB), иначе в хвост flash.

use core::ops::Range;

use adapters::flash::{Paged, whole};
use embassy_embedded_hal::adapter::BlockingAsync;

use crate::flash::FlashMutex;

/// Страница раздела `CONFIG` — размер сектора, в котором он лежит.
///
/// `sequential-storage` делит раздел на страницы по `ERASE_SIZE` флеша, а
/// цельный `Flash` объявляет им самый крупный сектор чипа. Раздел в мелких
/// секторах (F2/F4/F7 с OTA) со страницей всего чипа не делится на две, поэтому
/// страница своя (`Paged`); в хвосте flash она совпадает со страницей чипа.
/// Число посчитано при генерации тем же расчётом, что адреса в `memory.x`.
const CONFIG_ERASE_SIZE: usize = {{config_erase_size}};

/// Хранилище настроек этой платы — то, что лежит полем `Board` и уезжает в
/// задачи. Псевдоним по общему правилу шаблона: тип в сигнатуре
/// `#[embassy_executor::task]` должен быть конкретным.
pub type Settings =
    adapters::settings::Settings<BlockingAsync<Paged<crate::flash::Whole, CONFIG_ERASE_SIZE>>>;

// Ещё один `#[expect(unsafe_code)]` в `crates-cross` (есть только при `config=internal`):
// причина та же, что у блока в `flash.rs` — адрес линкерного символа иначе не взять.
#[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
unsafe extern "C" {
    /// Границы раздела `CONFIG` из `memory.x`, отсчитанные от базы flash —
    /// именно так их ждёт `embassy_stm32::flash::Flash` (у него нулевое
    /// смещение это база, а не адрес в адресном пространстве).
    static __config_start: u32;
    static __config_end: u32;
}

/// Собирает хранилище над разделом `CONFIG`.
///
/// # Паника
///
/// Если раздел не годится под `sequential-storage` — см.
/// `adapters::settings::Settings::new`.
pub(crate) fn new(flash: &'static FlashMutex) -> Settings {
    adapters::settings::Settings::new(BlockingAsync::new(Paged::new(whole(flash))), flash_range())
}

/// Границы раздела по символам линкера.
fn flash_range() -> Range<u32> {
    // SAFETY: символы объявлены линкерным скриптом как абсолютные адреса;
    // читается их адрес, а не содержимое (памяти за ними нет).
    let start = &raw const __config_start as u32;
    let end = &raw const __config_end as u32;
    start..end
}
{%- elsif config_store == "eeprom" %}
//!
//! Здесь — внутренняя EEPROM: флеш под настройки не тратится вовсе. EEPROM
//! пишется через тот же контроллер, что и флеш, поэтому доступ к ней — через
//! общий [`FlashMutex`] (`embassy_stm32::flash::Flash::eeprom_*`), по операции
//! под замком. Под хранилище отдана вся EEPROM, округлённая до страницы
//! обёртки (`adapters::eeprom::Eeprom::PAGE`).

use adapters::eeprom::{Eeprom, EepromAccess};
use static_cell::StaticCell;

use crate::flash::FlashMutex;

/// Хранилище настроек этой платы — то, что лежит полем `Board` и уезжает в
/// задачи. Псевдоним по общему правилу шаблона: тип в сигнатуре
/// `#[embassy_executor::task]` должен быть конкретным.
pub type Settings = adapters::settings::Settings<Eeprom<'static, EepromFlash>>;

/// Общий [`FlashMutex`] как источник EEPROM. Свой тип, а не `impl` на самом
/// мьютексе: и трейт `EepromAccess`, и `Mutex` чужие для `bsp`.
pub struct EepromFlash(&'static FlashMutex);

// Вручную: у `Flash` из `embassy-stm32` нет `Debug`.
impl core::fmt::Debug for EepromFlash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EepromFlash").finish_non_exhaustive()
    }
}

impl EepromAccess for EepromFlash {
    type Error = embassy_stm32::flash::Error;

    fn size(&self) -> u32 {
        // `usize` на Cortex-M — 32 бита, усечения нет.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "`usize` на Cortex-M — 32 бита"
        )]
        let size = embassy_stm32::flash::EEPROM_SIZE as u32;
        size
    }

    fn read(&self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.0
            .lock(|flash| flash.borrow_mut().eeprom_read_slice(offset, buf))
    }

    fn write(&self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        self.0
            .lock(|flash| flash.borrow_mut().eeprom_write_slice(offset, data))
    }
}

/// `&'static` — `Eeprom` держит доступ к EEPROM ссылкой, а хранилище уезжает в
/// задачу.
static ACCESS: StaticCell<EepromFlash> = StaticCell::new();

/// Собирает хранилище над всей внутренней EEPROM.
///
/// # Паника
///
/// Если EEPROM меньше двух страниц обёртки — см.
/// `adapters::settings::Settings::new`; у STM32L0/L1 таких нет.
pub(crate) fn new(flash: &'static FlashMutex) -> Settings {
    let eeprom = Eeprom::new(ACCESS.init(EepromFlash(flash)));
    let size = eeprom.usable();
    adapters::settings::Settings::new(eeprom, 0..size)
}
{%- elsif config_store == "external" and ota == "true" and ota_store == "external" %}
//!
//! Здесь — внешняя флеш, та же, что у OTA (`crate::external`): раздел под
//! настройки лежит на ней за `DFU`, границы — символами `__config_*` в
//! `memory.x` (смещения от начала микросхемы). Драйвер подставляется там же,
//! в `crate::external`.

use core::ops::Range;

use crate::external::{ExternalMutex, ExternalPartition};

/// Хранилище настроек этой платы — то, что лежит полем `Board` и уезжает в
/// задачи. Псевдоним по общему правилу шаблона: тип в сигнатуре
/// `#[embassy_executor::task]` должен быть конкретным.
pub type Settings = adapters::settings::Settings<ExternalPartition>;

// Ещё один `#[expect(unsafe_code)]` в `crates-cross`: причина та же, что у
// блока в `flash.rs` — адрес линкерного символа иначе не взять.
#[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
unsafe extern "C" {
    /// Границы раздела настроек на внешней флеши, от её начала.
    static __config_start: u32;
    static __config_end: u32;
}

/// Собирает хранилище над разделом внешней флеши.
///
/// # Паника
///
/// Если раздел не годится под `sequential-storage` — см.
/// `adapters::settings::Settings::new`.
pub(crate) fn new(external: &'static ExternalMutex) -> Settings {
    let range = flash_range();
    let partition = ExternalPartition::new(external, range.start, range.end - range.start);
    adapters::settings::Settings::new(partition, 0..range.end - range.start)
}

/// Границы раздела по символам линкера.
fn flash_range() -> Range<u32> {
    // SAFETY: символы объявлены линкерным скриптом как абсолютные значения;
    // читается их адрес, а не содержимое (памяти за ними нет).
    let start = &raw const __config_start as u32;
    let end = &raw const __config_end as u32;
    start..end
}
{%- elsif config_store == "external" %}
//!
//! Здесь — внешняя флеш. Какая микросхема стоит на плате, шаблон не знает и не
//! гадает: на месте драйвера заглушка [`ExternalFlash`] (=
//! `adapters::flash::Unconnected`). С ней проект собирается и работает, но
//! каждая операция хранилища отвечает ошибкой — настройки не сохраняются.
//!
//! Подключить свою микросхему — две правки в этом файле:
//!
//! 1. `ExternalFlash` — тип вашего драйвера. Годится любой, реализующий
//!    `embedded_storage_async::nor_flash::NorFlash`: готовый крейт
//!    (`spi-memory-async` для серии 25, `w25q32jv` и подобные) или свой.
//! 2. `new` — собрать драйвер из периферии (SPI и вывод CS; какие выводы умеет
//!    блок — `cargo xtask pins SPI1 --snippet`) и указать диапазон под
//!    настройки: не меньше двух секторов стирания микросхемы.
//!
//! Аргументы задач embassy обязаны быть `'static`, а драйвер — конкретным
//! типом: `Settings` остаётся псевдонимом, как и сейчас.

/// Драйвер внешней флеши — замените на свой.
pub type ExternalFlash = adapters::flash::Unconnected;

/// Хранилище настроек этой платы — то, что лежит полем `Board` и уезжает в
/// задачи. Псевдоним по общему правилу шаблона: тип в сигнатуре
/// `#[embassy_executor::task]` должен быть конкретным.
pub type Settings = adapters::settings::Settings<ExternalFlash>;

/// Собирает хранилище над внешней флешью. Сейчас — над заглушкой: о том, что
/// драйвер не подставлен, говорит одна строка в лог при старте.
pub(crate) fn new() -> Settings {
    defmt::warn!("bsp: внешняя флеш не подключена — настройки не сохраняются (bsp/src/config.rs)");
    adapters::settings::Settings::new(
        adapters::flash::Unconnected,
        0..adapters::flash::Unconnected::CAPACITY,
    )
}
{%- endif %}
