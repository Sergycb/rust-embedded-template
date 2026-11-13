#![doc = include_str!("../../../docs/modules/bsp-external.md")]

use core::any::TypeId;

{%- if ota == "true" and ota_store == "external" %}
use embassy_embedded_hal::flash::partition::Partition;
{%- endif %}
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use static_cell::StaticCell;
{%- if ota == "true" and ota_store == "external" %}

/// Страница обмена, под которую посчитана раскладка; bootloader берёт её
/// отсюда — переменная среды `EXT_PAGE_SIZE` из `[env]`
/// `crates-cross/.cargo/config.toml`. С драйвером, чьё стирание её не делит,
/// страница обмена `embassy-boot` выросла бы, `ACTIVE` перестал бы быть ей
/// кратен, и bootloader паниковал бы в `assert_partitions` на каждом старте.
pub const EXT_PAGE_SIZE: usize = match usize::from_str_radix(env!("EXT_PAGE_SIZE"), 10) {
    Ok(size) => size,
    Err(_) => panic!("EXT_PAGE_SIZE в [env] crates-cross/.cargo/config.toml — не число"),
};

// Вдобавок к проверке в bootloader'е: у приложения свой тип драйвера
// (асинхронный), и расходиться они могут.
const _: () = assert!(
    EXT_PAGE_SIZE.is_multiple_of(flash_page::erase_size::<ExternalFlash>()),
    "стирание драйвера внешней флеши не делит страницу обмена (EXT_PAGE_SIZE)"
);
{%- endif %}

/// Что не работает, пока флеш не отвечает, — для строки в лог.
const LOST: &str = "{% if ota == "true" and ota_store == "external" %}OTA{% if config_store == "external" %} и настройки не работают{% else %} не работает{% endif %}{% else %}настройки не сохраняются{% endif %}";

/// Шина-заглушка: микросхема не подключена. Драйвер W25Q над ней настоящий,
/// а каждая команда отвечает [`NotConnected`]: `init` не проходит, и драйвер
/// отказывает в любой операции — {% if ota == "true" and ota_store == "external" %}образ не принимается{% endif %}{% if ota == "true" and ota_store == "external" and config_store == "external" %}, {% endif %}{% if config_store == "external" %}настройки не сохраняются{% endif %}, но ничего не
/// падает. Асинхронная — для приложения, блокирующая — для bootloader'а: тот
/// с ней не обменивает разделы, а загружает текущий образ.
///
/// Плата подставляет сюда {% if ext_flash_bus == "spi" %}`w25q::spi::SpiAdapter` над `SpiDevice` блока `{{ext_flash_block}}`{% else %}свою обёртку
/// над `embassy_stm32::{% if ext_flash_bus == "quadspi" %}qspi::Qspi{% else %}ospi::Ospi{% endif %}` блока `{{ext_flash_block}}`{% endif %}
/// (docs/modules/bsp-external.md).
#[derive(Debug, Default)]
pub struct StubBus;

/// Ответ [`StubBus`] на любую команду.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub struct NotConnected;

impl w25q::bus::FlashBusAsync for StubBus {
    type Error = NotConnected;

    /// Ширина настоящей шины: по ней драйвер выбирает команды{% if ext_flash_bus != "spi" %} (и ставит
    /// бит QE){% endif %}.
    const LANES: w25q::bus::Lanes = w25q::bus::Lanes::{% if ext_flash_bus == "spi" %}Single{% else %}Quad{% endif %};

    fn transfer(
        &mut self,
        _: &w25q::bus::Command,
        _: w25q::bus::Data<'_>,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Err(NotConnected))
    }
}

impl w25q::bus::FlashBusSync for StubBus {
    type Error = NotConnected;

    const LANES: w25q::bus::Lanes = <Self as w25q::bus::FlashBusAsync>::LANES;

    fn transfer(
        &mut self,
        _: &w25q::bus::Command,
        _: w25q::bus::Data<'_>,
    ) -> Result<(), Self::Error> {
        Err(NotConnected)
    }
}

/// Шина микросхемы: по умолчанию — [`StubBus`]; плата подставляет свою.
pub type Bus = StubBus;

/// Драйвер внешней флеши: асинхронный W25Q над [`Bus`].
pub type ExternalFlash = w25q::device::W25qAsync<Bus, embassy_time::Delay>;

/// Внешняя флеш под замком — то, над чем строятся её разделы.
///
/// `NoopRawMutex` — то же допущение «один исполнитель», что у
/// `crate::flash::Lock`; менять вместе.
pub type ExternalMutex = Mutex<NoopRawMutex, ExternalFlash>;

{%- if ota == "true" and ota_store == "external" %}

/// Разделы обновления на внешней флеши — рычаг под адаптером обновления
/// (`domain::ota::Updater`). `DFU` и
/// `BOOTLOADER_STATE` нарезает плата по символам `memory.x` (`crate::layout`)
/// — смещениям от начала микросхемы; оба над одним драйвером: команды разных
/// разделов не должны перемежаться.
pub type FirmwareFlash = ports::Partitions<
    Partition<'static, NoopRawMutex, ExternalFlash>,
    Partition<'static, NoopRawMutex, ExternalFlash>,
>;
{%- endif %}
{%- if config == "true" and config_store == "external" %}

/// Раздел настроек на внешней флеши — носитель хранилища настроек
/// (`domain::settings`).
pub type ConfigFlash =
    embassy_embedded_hal::flash::partition::Partition<'static, NoopRawMutex, ExternalFlash>;
{%- endif %}

static EXTERNAL: StaticCell<ExternalMutex> = StaticCell::new();

/// Подставлена ли шина вместо заглушки — для target-тестов, которым без
/// микросхемы проверять нечего.
#[must_use]
pub fn connected() -> bool {
    TypeId::of::<Bus>() != TypeId::of::<StubBus>()
}

/// Собирает драйвер и опознаёт микросхему (`init`: JEDEC ID, объём{% if ext_flash_bus != "spi" %}, бит QE{% endif %}).
///
/// Не прошёл `init` или микросхема меньше разделов (`crate::layout::ext_flash_end`) — одна строка в
/// лог, и драйвер отказывает в любой операции ошибкой, а не паникой. Плата без
/// флеши (или с заглушкой шины) работает, только без того, что на флеши лежит.
pub(crate) async fn new(_resources: crate::resources::ExtFlashResources) -> &'static ExternalMutex {
    let mut flash = ExternalFlash::new(Bus, embassy_time::Delay);
    let end = crate::layout::ext_flash_end();
    let flash = match flash.init().await {
        Ok(_) if flash.capacity() >= end => {
            defmt::info!(
                "bsp: внешняя флеш W25Q, {=u32} KiB",
                flash.capacity() / 1024
            );
            flash
        }
        Ok(_) => {
            defmt::error!(
                "bsp: внешняя флеш {=u32} KiB, а разделам нужно {=u32} KiB — {=str}",
                flash.capacity() / 1024,
                end / 1024,
                LOST
            );
{%- if ota == "true" and ota_store == "external" %}
            // Не оставлять опознанный драйвер: с ним образ лёг бы в начало
            // `DFU`, которого на микросхеме не хватает, а bootloader на обмене
            // упёрся бы в её конец. Неопознанный отказывает во всём.
{%- else %}
            // Не оставлять опознанный драйвер: с ним раздел настроек упёрся бы
            // в конец микросхемы. Неопознанный отказывает во всём.
{%- endif %}
            let (bus, delay) = flash.release();
            ExternalFlash::new(bus, delay)
        }
        Err(_) if !connected() => {
            defmt::warn!(
                "bsp: внешняя флеш не подключена — {=str} (bsp/src/external.rs)",
                LOST
            );
            flash
        }
        Err(err) => {
            defmt::error!("bsp: внешняя флеш не отвечает: {} — {=str}", err, LOST);
            flash
        }
    };
    EXTERNAL.init(Mutex::new(flash))
}
{%- if ota == "true" and ota_store == "external" %}

/// Нарезает разделы обновления на внешней флеши — по символам `memory.x`, как
/// их режет bootloader.
pub(crate) fn firmware(external: &'static ExternalMutex) -> FirmwareFlash {
    let (dfu_start, dfu_len) = crate::layout::dfu();
    let (state_start, state_len) = crate::layout::state();
    ports::Partitions {
        dfu: Partition::new(external, dfu_start, dfu_len),
        state: Partition::new(external, state_start, state_len),
        capacity: crate::layout::active_len(),
        layout: crate::layout::fingerprint(),
    }
}
{%- endif %}
{%- if config == "true" and config_store == "external" %}

/// Собирает раздел настроек на внешней флеши.
pub(crate) fn config(external: &'static ExternalMutex) -> ConfigFlash {
    let (start, len) = crate::layout::config();
    ConfigFlash::new(external, start, len)
}
{%- endif %}
