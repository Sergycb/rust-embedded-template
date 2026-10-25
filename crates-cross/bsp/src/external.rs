#![doc = include_str!("../../../docs/modules/bsp-external.md")]

use core::any::TypeId;

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

/// Конец последнего раздела на внешней флеши, байты: меньше микросхема не
/// годится. Переменная среды `EXT_FLASH_END` из `[env]`
/// `crates-cross/.cargo/config.toml` — тот же расчёт, что у раскладки.
pub const EXT_FLASH_END: u32 = match u32::from_str_radix(env!("EXT_FLASH_END"), 10) {
    Ok(end) => end,
    Err(_) => panic!("EXT_FLASH_END в [env] crates-cross/.cargo/config.toml — не число"),
};

/// Что не работает, пока флеш не отвечает, — для строки в лог.
const LOST: &str = "{% if ota == "true" and ota_store == "external" %}OTA{% if config_store == "external" %} и настройки не работают{% else %} не работает{% endif %}{% else %}настройки не сохраняются{% endif %}";

/// Шина микросхемы — пока заглушка. Плата подставляет сюда {% if ext_flash_bus == "spi" %}`w25q::spi::SpiAdapter` над `SpiDevice` блока `{{ext_flash_block}}`{% else %}свою обёртку
/// над `embassy_stm32::{% if ext_flash_bus == "quadspi" %}qspi::Qspi{% else %}ospi::Ospi{% endif %}` блока `{{ext_flash_block}}`{% endif %}
/// (docs/modules/bsp-external.md).
pub type Bus = crate::stub::FlashBus;

/// Драйвер внешней флеши: асинхронный W25Q над [`Bus`].
pub type ExternalFlash = w25q::device::W25qAsync<Bus, embassy_time::Delay>;

/// Внешняя флеш под замком — то, над чем строятся её разделы.
///
/// `NoopRawMutex` — то же допущение «один исполнитель», что у
/// `crate::flash::FlashMutex`; менять вместе.
pub type ExternalMutex = Mutex<NoopRawMutex, ExternalFlash>;

{%- if ota == "true" and ota_store == "external" %}

/// Флеш под обновлением — рычаг, над которым узел OTA собирает адаптер
/// (`adapters::ota::Updater`, порт `Assemble`). `DFU` и `BOOTLOADER_STATE`
/// находит в нём сам узел по символам `memory.x` (`adapters::ota::Linked`) —
/// смещениям от начала микросхемы; оба над одним драйвером: команды разных
/// разделов не должны перемежаться.
pub type FirmwareFlash = adapters::ota::Linked<ExternalFlash, ExternalFlash>;
{%- endif %}
{%- if config == "true" and config_store == "external" %}

/// Раздел настроек на внешней флеши — носитель, над которым узел-потребитель
/// собирает хранилище (`adapters::settings`, порт `Assemble`).
pub type ConfigFlash =
    embassy_embedded_hal::flash::partition::Partition<'static, NoopRawMutex, ExternalFlash>;
{%- endif %}

static EXTERNAL: StaticCell<ExternalMutex> = StaticCell::new();

/// Подставлена ли шина вместо заглушки — для target-тестов, которым без
/// микросхемы проверять нечего.
#[must_use]
pub fn connected() -> bool {
    TypeId::of::<Bus>() != TypeId::of::<crate::stub::FlashBus>()
}

/// Собирает драйвер и опознаёт микросхему (`init`: JEDEC ID, объём{% if ext_flash_bus != "spi" %}, бит QE{% endif %}).
///
/// Не прошёл `init` или микросхема меньше [`EXT_FLASH_END`] — одна строка в
/// лог, и драйвер отказывает в любой операции ошибкой, а не паникой. Плата без
/// флеши (или с заглушкой шины) работает, только без того, что на флеши лежит.
pub(crate) async fn new(_resources: crate::resources::ExtFlashResources) -> &'static ExternalMutex {
    let mut flash = ExternalFlash::new(crate::stub::FlashBus, embassy_time::Delay);
    let flash = match flash.init().await {
        Ok(_) if flash.capacity() >= EXT_FLASH_END => {
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
                EXT_FLASH_END / 1024,
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

/// Собирает флеш под обновлением над внешней флешью.
pub(crate) fn firmware(external: &'static ExternalMutex) -> FirmwareFlash {
    adapters::ota::Linked::new(external, external, crate::layout::ACTIVE_LEN)
}
{%- endif %}
{%- if config == "true" and config_store == "external" %}

/// Собирает раздел настроек на внешней флеши.
pub(crate) fn config(external: &'static ExternalMutex) -> ConfigFlash {
    ConfigFlash::new(
        external,
        crate::layout::CONFIG_START,
        crate::layout::CONFIG_END - crate::layout::CONFIG_START,
    )
}
{%- endif %}
