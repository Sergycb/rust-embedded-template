#![doc = include_str!("../../../docs/modules/bsp-ota.md")]

use core::convert::Infallible;

use embassy_boot::FirmwareUpdaterConfig;
use embassy_embedded_hal::flash::partition::BlockingPartition;
use embassy_stm32::flash::{Blocking, Flash};
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use ports::{Announce, ImageSource, Rejection};

use crate::FlashMutex;

/// Раздел поверх общего `Flash` чипа — то, из чего собирается адаптер.
///
/// Тот же цельный `Flash`, что и в `crates-cross/boot`: он сам знает реальные
/// границы секторов чипа, в том числе неравномерные (F4/F7/H7), а банковые
/// регионы у каждого семейства называются по-своему.
pub type Partition = BlockingPartition<'static, NoopRawMutex, Flash<'static, Blocking>>;

/// Адаптер обновления этой платы — порт `FirmwareUpdate` поверх разделов
/// `DFU`/`BOOTLOADER_STATE`. Псевдоним по общему правилу шаблона: тип слота
/// узла OTA должен быть конкретным, а адаптер приходит из `adapters` generic.
pub type Updater = adapters::ota::Updater<Partition, Partition>;

/// Всё, что плата отдаёт узлу OTA: канал доставки и адаптер разделов.
///
/// Своя структура, а не `domain::ota::Inputs`: `bsp` зависит только от
/// `ports` и `adapters`, про `domain` он не знает. Во вход узла её
/// раскладывает compose-site (`crates-cross/app/src/graph.rs`).
pub struct Ota {
    /// Канал доставки образа — реализация `ports::ImageSource`.
    pub link: Link,
    /// Адаптер разделов — реализация `ports::FirmwareUpdate`.
    pub flash: Updater,
}

/// Канал доставки образа этой платы — заглушка, которую проект заменяет
/// своим транспортом.
///
/// Каркас, как `domain::app::run`: узел `OTA` уже собран{% if graph == "true" %} и спавнится графом{% else %} — зовите
/// `domain::ota::run{% if signed == "true" %}_signed{% endif %}` сами из `main`{% endif %},
/// а как приходит образ — USB CDC, UART, сеть, SD-карта — шаблон не знает.
/// Заглушка ждёт заголовок вечно, то есть узел висит в `begin()` и ничего не
/// принимает; о себе она сообщает одной строкой в лог при старте.
///
/// Заменить — значит реализовать три метода `ports::ImageSource` на объекте,
/// собранном в `Board::new` из вашей периферии: `begin` — дождаться и
/// разобрать заголовок (длина{% if signed == "true" %}, подпись{% endif %}),
/// `next` — отдавать куски образа, `finish` — сообщить исход отправителю и
/// решить, перезапускать ли МК (`cortex_m::peripheral::SCB::sys_reset()`).
/// Формат пакетов и проверка целостности — ваши; узел про них не знает.
/// Всё, что не зависит от канала — сверка длины до стирания, буферизация до
/// слова флеша, порядок проверок подписи, — уже в `domain::ota`.
pub struct Link;

impl ImageSource for Link {
    type Error = Infallible;

    async fn begin(&mut self) -> Result<Announce, Self::Error> {
        defmt::warn!("bsp: транспорт OTA не реализован — узел OTA ждёт впустую");
        core::future::pending().await
    }

    async fn next(&mut self) -> Result<Option<&[u8]>, Self::Error> {
        Ok(None)
    }

    async fn finish(&mut self, _outcome: Result<(), Rejection>) -> Result<(), Self::Error> {
        Ok(())
    }
}

/// Собирает вход узла OTA: адаптер из разделов `DFU` и `BOOTLOADER_STATE`,
/// найденных по символам `memory.x`, и канал доставки.
pub(crate) fn new(flash: &'static FlashMutex) -> Ota {
    let config = FirmwareUpdaterConfig::from_linkerfile_blocking(flash, flash);
    Ota {
        link: Link,
        flash: Updater::new(config.dfu, config.state, max_image_len()),
    }
}

unsafe extern "C" {
    /// Границы раздела `ACTIVE` — те же символы, по которым разделы находит
    /// `FirmwareUpdaterConfig::from_linkerfile_blocking`.
    static __bootloader_active_start: u32;
    static __bootloader_active_end: u32;
}

/// Самый длинный образ, который вообще доедет до устройства — его отдаёт
/// порт методом `FirmwareUpdate::capacity`.
///
/// Это размер `ACTIVE`, а НЕ `DFU`, хотя принимается образ в `DFU`. Раздел
/// обновления по построению больше активного на одну-две страницы (этого
/// требует `assert_partitions` в `embassy-boot`: обмену нужна запасная
/// страница), а обмен копирует ровно `ACTIVE`. Образ размером между ними
/// прошёл бы и проверку длины, и подпись, и пометку к обмену — а в `ACTIVE`
/// приехал бы обрезанным: устройство отработало бы полный цикл обновления с
/// перезагрузкой и молча откатилось.
///
/// Функция, а не `const`: это разность адресов линкерных символов, а их
/// значения известны линкеру, а не компилятору.
fn max_image_len() -> u32 {
    // SAFETY: символы объявлены линкерным скриптом как абсолютные значения;
    // берётся их адрес, а не содержимое.
    let start = &raw const __bootloader_active_start as u32;
    let end = &raw const __bootloader_active_end as u32;
    end.saturating_sub(start)
}
