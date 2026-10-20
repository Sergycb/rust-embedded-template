//! Внешняя флеш платы: на ней `STATE` и `DFU` обновления{% if config_store == "external" %} и раздел
//! настроек{% endif %} (`ota=external`).
//!
//! Какая микросхема стоит на плате, шаблон не знает и не гадает: на месте
//! драйвера заглушка [`ExternalFlash`] (= `crate::stub::Flash`). С
//! ней проект собирается и работает, но каждая операция с внешней флешью
//! отвечает ошибкой: образ не принимается, а bootloader, не дозвавшись до
//! своего драйвера, загружает текущий образ.
//!
//! Подключить свою микросхему — две правки в этом файле и одна в
//! bootloader'е (тип — `crates-cross/boot/src/external.rs`, сборка из
//! периферии — `crates-cross/boot/src/main.rs`):
//!
//! 1. [`ExternalFlash`] — тип вашего драйвера. Годится любой, реализующий
//!    `embedded_storage_async::nor_flash::NorFlash`: готовый крейт
//!    (`spi-memory-async` для серии 25, `w25q32jv` и подобные) или свой.
//! 2. [`new`] — собрать драйвер из периферии (SPI и вывод CS; какие выводы
//!    умеет блок — `cargo xtask pins SPI1 --snippet`).
//! 3. В bootloader'е — та же микросхема, но драйвер блокирующий
//!    (`embedded_storage::nor_flash::NorFlash`): исполнителя там нет.
//!
//! Требование к драйверу одно: его страница стирания делит страницу обмена,
//! под которую посчитана раскладка (`EXT_PAGE_SIZE` — здесь и в bootloader'е,
//! оба проверяют это при компиляции). Сектор 4 KiB есть у всех
//! распространённых SPI NOR.
//!
//! Разделы — одним объектом под одним мьютексом: микросхема одна, и операции
//! разных разделов не должны перемежаться посреди команды SPI.

use core::any::TypeId;

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use static_cell::StaticCell;

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

static EXTERNAL: StaticCell<ExternalMutex> = StaticCell::new();

/// Драйвер внешней флеши — замените на свой.
pub type ExternalFlash = crate::stub::Flash;

/// Внешняя флеш под замком — то, над чем строятся её разделы.
///
/// `NoopRawMutex` — то же допущение «один исполнитель», что у
/// `crate::flash::FlashMutex`; менять вместе.
pub type ExternalMutex = Mutex<NoopRawMutex, ExternalFlash>;

/// Раздел внешней флеши.
pub type ExternalPartition =
    embassy_embedded_hal::flash::partition::Partition<'static, NoopRawMutex, ExternalFlash>;

/// Подставлен ли драйвер вместо заглушки — для target-тестов, которым без
/// микросхемы проверять нечего.
#[must_use]
pub fn connected() -> bool {
    TypeId::of::<ExternalFlash>() != TypeId::of::<crate::stub::Flash>()
}

/// Собирает драйвер. Сейчас — заглушку: о том, что драйвер не подставлен,
/// говорит одна строка в лог при старте.
pub(crate) fn new() -> &'static ExternalMutex {
{%- if config_store == "external" %}
    defmt::warn!(
        "bsp: внешняя флеш не подключена — OTA и настройки не работают (bsp/src/external.rs)"
    );
{%- else %}
    defmt::warn!("bsp: внешняя флеш не подключена — OTA не работает (bsp/src/external.rs)");
{%- endif %}
    EXTERNAL.init(Mutex::new(crate::stub::Flash))
}
