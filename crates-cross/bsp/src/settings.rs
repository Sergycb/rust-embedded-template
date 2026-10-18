//! Обвязка хранилища настроек этой платы: на чём оно лежит и псевдоним типа
//! для задач.
//!
//! Само хранилище — `adapters::settings::Settings` (generic по флешу,
//! тестируется на хосте); ему годится любой асинхронный `NorFlash`. Здесь
//! остаётся то, что знает про плату: что именно в этот слот подставлено. При
//! генерации выбрано одно из трёх (`config` в cargo-generate, см.
//! docs/flash.md):
//!
//! - раздел во внутреннем флеше — границы `CONFIG` приезжают переменными
//!   среды (`CONFIG_START`/`CONFIG_END`), страница — [`CONFIG_ERASE_SIZE`];
//! - внутренняя EEPROM (STM32L0/L1) — через `adapters::eeprom::Eeprom`;
//! - внешняя флеш — ваш драйвер; пока на его месте заглушка
//!   `adapters::flash::Unconnected`.
{%- if config_store == "flash" %}
//!
//! Здесь — раздел во внутреннем флеше: две страницы стирания (меньше
//! `sequential-storage` не берёт), положенные при генерации туда, где они
//! дешевле: с OTA на F2/F4/F7 — в мелкие сектора перед `ACTIVE` (у F4 —
//! 2×16 KiB), иначе в хвост flash.

use adapters::flash::ErasePage;
use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_embedded_hal::flash::partition::BlockingPartition;

use crate::flash::{FlashMutex, SIZE, Whole};

/// Страница раздела `CONFIG` — размер сектора, в котором он лежит.
///
/// `sequential-storage` делит раздел на страницы по `ERASE_SIZE` флеша, а
/// цельный `Flash` объявляет им самый крупный сектор чипа. Раздел в мелких
/// секторах (F2/F4/F7 с OTA) со страницей всего чипа не делится на две, поэтому
/// страница своя (`ErasePage`); в хвосте flash она совпадает со страницей чипа.
/// Число посчитано при генерации тем же расчётом, что адреса в `memory.x`.
const CONFIG_ERASE_SIZE: usize = {{config_erase_size}};

/// Начало раздела `CONFIG`: смещение от базы flash, `memory.x` считает его тем
/// же расчётом. Приезжает переменной среды `CONFIG_START` из `[env]`
/// `crates-cross/.cargo/config.toml`, а не линкерным символом (`unsafe extern`).
const CONFIG_START: u32 = match u32::from_str_radix(env!("CONFIG_START"), 10) {
    Ok(start) => start,
    Err(_) => panic!("CONFIG_START в [env] crates-cross/.cargo/config.toml — не число"),
};

/// Конец раздела `CONFIG` — см. [`CONFIG_START`].
const CONFIG_END: u32 = match u32::from_str_radix(env!("CONFIG_END"), 10) {
    Ok(end) => end,
    Err(_) => panic!("CONFIG_END в [env] crates-cross/.cargo/config.toml — не число"),
};

/// Хранилище настроек этой платы — то, что лежит полем `Board` и уезжает в
/// задачи. Псевдоним по общему правилу шаблона: тип в сигнатуре
/// `#[embassy_executor::task]` должен быть конкретным.
pub type Settings =
    adapters::settings::Settings<BlockingAsync<ErasePage<Whole, CONFIG_ERASE_SIZE>>>;

/// Собирает хранилище над разделом `CONFIG`.
///
/// # Паника
///
/// Если раздел не годится под `sequential-storage` — см.
/// `adapters::settings::Settings::new`.
pub(crate) fn new(flash: &'static FlashMutex) -> Settings {
    adapters::settings::Settings::new(
        BlockingAsync::new(ErasePage::new(BlockingPartition::new(flash, 0, SIZE))),
        CONFIG_START..CONFIG_END,
    )
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

/// `&'static` — `Eeprom` держит доступ к EEPROM ссылкой, а хранилище уезжает в
/// задачу.
static ACCESS: StaticCell<EepromFlash> = StaticCell::new();

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
//! настройки лежит на ней за `DFU`, границы — переменные среды
//! `CONFIG_START`/`CONFIG_END` (смещения от начала микросхемы). Драйвер подставляется там же,
//! в `crate::external`.

use crate::external::{ExternalMutex, ExternalPartition};

/// Начало раздела `CONFIG`: смещение от базы flash, `memory.x` считает его тем
/// же расчётом. Приезжает переменной среды `CONFIG_START` из `[env]`
/// `crates-cross/.cargo/config.toml`, а не линкерным символом (`unsafe extern`).
const CONFIG_START: u32 = match u32::from_str_radix(env!("CONFIG_START"), 10) {
    Ok(start) => start,
    Err(_) => panic!("CONFIG_START в [env] crates-cross/.cargo/config.toml — не число"),
};

/// Конец раздела `CONFIG` — см. [`CONFIG_START`].
const CONFIG_END: u32 = match u32::from_str_radix(env!("CONFIG_END"), 10) {
    Ok(end) => end,
    Err(_) => panic!("CONFIG_END в [env] crates-cross/.cargo/config.toml — не число"),
};

/// Хранилище настроек этой платы — то, что лежит полем `Board` и уезжает в
/// задачи. Псевдоним по общему правилу шаблона: тип в сигнатуре
/// `#[embassy_executor::task]` должен быть конкретным.
pub type Settings = adapters::settings::Settings<ExternalPartition>;

/// Собирает хранилище над разделом внешней флеши.
///
/// # Паника
///
/// Если раздел не годится под `sequential-storage` — см.
/// `adapters::settings::Settings::new`.
pub(crate) fn new(external: &'static ExternalMutex) -> Settings {
    let size = CONFIG_END - CONFIG_START;
    let partition = ExternalPartition::new(external, CONFIG_START, size);
    adapters::settings::Settings::new(partition, 0..size)
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
    defmt::warn!("bsp: внешняя флеш не подключена — настройки не сохраняются (bsp/src/settings.rs)");
    adapters::settings::Settings::new(
        adapters::flash::Unconnected,
        0..adapters::flash::Unconnected::CAPACITY,
    )
}
{%- endif %}
