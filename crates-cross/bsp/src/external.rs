//! Внешняя флеш платы: на ней `STATE` и `DFU` обновления{% if config_store == "external" %} и раздел
//! настроек{% endif %} (`ota=external`).
//!
//! Какая микросхема стоит на плате, шаблон не знает и не гадает: на месте
//! драйвера заглушка [`ExternalFlash`] (= `adapters::flash::Unconnected`). С
//! ней проект собирается и работает, но каждая операция с внешней флешью
//! отвечает ошибкой: образ не принимается, а bootloader, не дозвавшись до
//! своего драйвера, загружает текущий образ.
//!
//! Подключить свою микросхему — две правки в этом файле и одна в
//! `crates-cross/boot/src/main.rs`:
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
//! под которую посчитана раскладка (`EXT_PAGE_SIZE` в bootloader'е, там же
//! проверка при компиляции). Сектор 4 KiB есть у всех распространённых SPI NOR.
//!
//! Разделы — одним объектом под одним мьютексом: микросхема одна, и операции
//! разных разделов не должны перемежаться посреди команды SPI.

use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use static_cell::StaticCell;

/// Драйвер внешней флеши — замените на свой.
pub type ExternalFlash = adapters::flash::Unconnected;

/// Внешняя флеш под замком — то, над чем строятся её разделы.
///
/// `NoopRawMutex` — то же допущение «один исполнитель», что у
/// `crate::board::FlashMutex`; менять вместе.
pub type ExternalMutex = Mutex<NoopRawMutex, ExternalFlash>;

/// Раздел внешней флеши.
pub type ExternalPartition =
    embassy_embedded_hal::flash::partition::Partition<'static, NoopRawMutex, ExternalFlash>;

static EXTERNAL: StaticCell<ExternalMutex> = StaticCell::new();

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
    EXTERNAL.init(Mutex::new(adapters::flash::Unconnected))
}
