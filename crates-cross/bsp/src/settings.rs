//! Носитель хранилища настроек этой платы — рычаг `NorFlash`: на чём лежат
//! настройки.
//!
//! Само хранилище — `adapters::settings::Settings` (generic по флешу,
//! тестируется на хосте); ему годится любой асинхронный `NorFlash` на весь
//! его объём, и собирает его узел-потребитель (порт `Assemble`). Здесь
//! остаётся то, что знает про плату: что именно в этот слот подставлено. При
//! генерации выбрано одно из трёх (`config` в cargo-generate, см.
//! docs/flash.md):
//!
//! - раздел во внутреннем флеше — границы `CONFIG` приезжают переменными
//!   среды (`CONFIG_START`/`CONFIG_END`), страница — [`CONFIG_ERASE_SIZE`];
//! - внутренняя EEPROM (STM32L0/L1) — через `adapters::eeprom::Eeprom`;
//! - внешняя флеш W25Q — драйвер `crate::external`, шина под ним — заглушка,
//!   пока плата её не подставит.
{%- if config_store == "flash" %}
//!
//! Здесь — раздел во внутреннем флеше: две страницы стирания (меньше
//! `sequential-storage` не берёт), положенные при генерации туда, где они
//! дешевле: с OTA на F2/F4/F7 — в мелкие сектора перед `ACTIVE` (у F4 —
//! 2×16 KiB), иначе в хвост flash.

use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_embedded_hal::flash::partition::{BlockingPartition, Partition};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
{%- if config_paged == "true" %}
use flash_page::ErasePage;
{%- endif %}
use static_cell::StaticCell;

use crate::flash::{FlashMutex, SIZE, Whole};

/// Страница раздела `CONFIG` — размер сектора, в котором он лежит.
///
/// `sequential-storage` делит раздел на страницы по `ERASE_SIZE` флеша, а
/// цельный `Flash` объявляет им самый крупный сектор чипа. Раздел в мелких
/// секторах (F2/F4/F7 с OTA) со страницей всего чипа не делится на две, поэтому
/// страница своя{% if config_paged == "true" %} (`ErasePage`){% endif %}; в хвосте flash она совпадает со страницей чипа{% if config_paged != "true" %} — как у
/// этого чипа: раздел строится прямо над `Whole`, а равенство держит
/// `const`-assert ниже{% endif %}.
/// Число посчитано при генерации тем же расчётом, что адреса в `memory.x`.
const CONFIG_ERASE_SIZE: usize = {{config_erase_size}};
{%- if config_paged != "true" %}

const _: () = assert!(
    CONFIG_ERASE_SIZE == embassy_stm32::flash::MAX_ERASE_SIZE,
    "CONFIG_ERASE_SIZE из генерации не равен странице цельного `Flash` \
     (embassy_stm32::flash::MAX_ERASE_SIZE): `CONFIG` нужна своя страница"
);
{%- endif %}

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

// Раздел строится как `Partition` над страницей [`CONFIG_ERASE_SIZE`], и
// `Partition::new` паникует на старте, если его границы не кратны ей. Расчёт
// раскладки в генерации держит их на границах, а это — проверка при компиляции,
// чтобы ручная правка `[env]` не превратилась в панику на плате.
const _: () = assert!(
    (CONFIG_START as usize).is_multiple_of(CONFIG_ERASE_SIZE)
        && (CONFIG_END as usize).is_multiple_of(CONFIG_ERASE_SIZE),
    "раздел CONFIG не на границах своей страницы (CONFIG_ERASE_SIZE): Partition::new паникует"
);

/// Общий `Flash` чипа, видимый асинхронно{% if config_paged == "true" %}, со страницей сектора `CONFIG`
/// ([`CONFIG_ERASE_SIZE`], `ErasePage`){% endif %} — то, над чем строится раздел.
{%- if config_paged == "true" %}
type ConfigBacking = BlockingAsync<ErasePage<Whole, CONFIG_ERASE_SIZE>>;
{%- else %}
type ConfigBacking = BlockingAsync<Whole>;
{%- endif %}

/// Свой асинхронный мьютекс разделу: `Partition` строится из `&Mutex<_, T>`.
/// Внутри — ссылка на общий [`FlashMutex`], а не второй `Flash`: контроллер на
/// чипе один. `NoopRawMutex` — то же допущение «один исполнитель», что у
/// `FlashMutex` и разделов OTA; менять вместе.
static CONFIG_FLASH: StaticCell<Mutex<NoopRawMutex, ConfigBacking>> = StaticCell::new();

/// Раздел `CONFIG` — то, что лежит полем `Board` и над чем узел собирает
/// хранилище. Именно раздел, а не весь флеш: запись за его границы
/// отказывает, а не стирает `ACTIVE` или bootloader.
pub type ConfigFlash = Partition<'static, NoopRawMutex, ConfigBacking>;

/// Собирает раздел `CONFIG`. Его начало кратно странице [`ConfigBacking`]{% if config_paged != "true" %}
/// (`CONFIG_ERASE_SIZE` равен странице цельного `Flash` — `const`-assert выше){% endif %}:
/// иначе `Partition::new` паникует.
pub(crate) fn new(flash: &'static FlashMutex) -> ConfigFlash {
{%- if config_paged == "true" %}
    let backing = BlockingAsync::new(ErasePage::new(BlockingPartition::new(flash, 0, SIZE)));
{%- else %}
    let backing = BlockingAsync::new(BlockingPartition::new(flash, 0, SIZE));
{%- endif %}
    let backing = CONFIG_FLASH.init(Mutex::new(backing));
    Partition::new(backing, CONFIG_START, CONFIG_END - CONFIG_START)
}
{%- elsif config_store == "eeprom" %}
//!
//! Здесь — внутренняя EEPROM: флеш под настройки не тратится вовсе. EEPROM
//! пишется через тот же контроллер, что и флеш, поэтому доступ к ней — через
//! общий [`FlashMutex`] (`embassy_stm32::flash::Flash::eeprom_*`), по операции
//! под замком. Наружу EEPROM уходит тем же рычагом, что и флеш, — `NorFlash`:
//! обёртку `adapters::eeprom::Eeprom` (инверсия байтов, страница) собирает
//! плата, и под хранилище отдана вся EEPROM, округлённая до её страницы.

use ports::EepromAccess;
use static_cell::StaticCell;

use crate::flash::FlashMutex;

/// `&'static` — обёртка `Eeprom` держит доступ к EEPROM ссылкой, а хранилище
/// уезжает в задачу.
static ACCESS: StaticCell<EepromFlash> = StaticCell::new();

/// EEPROM как `NorFlash` — то, что лежит полем `Board` и над чем узел
/// собирает хранилище.
pub type ConfigFlash = adapters::eeprom::Eeprom<'static, EepromFlash>;

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

/// Собирает внутреннюю EEPROM как `NorFlash`.
pub(crate) fn new(flash: &'static FlashMutex) -> ConfigFlash {
    adapters::eeprom::Eeprom::new(ACCESS.init(EepromFlash(flash)))
}
{%- elsif config_store == "external" %}
//!
//! Здесь — внешняя флеш W25Q (`crate::external`){% if ota == "true" and ota_store == "external" %}, та же, что у OTA: раздел под
//! настройки лежит на ней за `DFU`{% else %}: раздел под настройки — с её
//! начала{% endif %}. Границы — переменные среды `CONFIG_START`/`CONFIG_END`
//! (смещения от начала микросхемы); шину микросхемы плата подставляет в
//! `crate::external`.

use crate::external::{ExternalMutex, ExternalPartition};

/// Начало раздела настроек — смещение от начала внешней флеши, посчитанное
/// генерацией. Приезжает переменной среды `CONFIG_START` из `[env]`
/// `crates-cross/.cargo/config.toml`.
const CONFIG_START: u32 = match u32::from_str_radix(env!("CONFIG_START"), 10) {
    Ok(start) => start,
    Err(_) => panic!("CONFIG_START в [env] crates-cross/.cargo/config.toml — не число"),
};

/// Конец раздела настроек — см. [`CONFIG_START`].
const CONFIG_END: u32 = match u32::from_str_radix(env!("CONFIG_END"), 10) {
    Ok(end) => end,
    Err(_) => panic!("CONFIG_END в [env] crates-cross/.cargo/config.toml — не число"),
};

/// Раздел настроек на внешней флеши — то, что лежит полем `Board` и над чем
/// узел собирает хранилище.
pub type ConfigFlash = ExternalPartition;

/// Собирает раздел настроек на внешней флеши.
pub(crate) fn new(external: &'static ExternalMutex) -> ConfigFlash {
    ExternalPartition::new(external, CONFIG_START, CONFIG_END - CONFIG_START)
}
{%- endif %}
