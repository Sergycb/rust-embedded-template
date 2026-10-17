#![doc = include_str!("../../../docs/modules/bsp-ota.md")]

use core::convert::Infallible;

{%- if ota_store == "external" %}

use embassy_boot::FirmwareUpdaterConfig;
use ports::{Announce, ImageSource, Rejection};

use crate::external::{ExternalMutex, ExternalPartition};

/// Раздел `DFU` — на внешней флеши (`crate::external`).
pub type Partition = ExternalPartition;

/// Раздел `BOOTLOADER_STATE` — на той же внешней флеши: его страницу несёт
/// драйвер.
pub type StatePartition = ExternalPartition;
{%- else %}

use adapters::flash::{Paged, Shared};
use embassy_boot::FirmwareUpdaterConfig;
use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use ports::{Announce, ImageSource, Rejection};
use static_cell::StaticCell;

use crate::board::{% if dfu_bank == "true" %}{DfuBankMutex, FlashMutex}{% else %}FlashMutex{% endif %};

/// Флеш под разделом `DFU`: общий `Flash` чипа, видимый асинхронно
/// (`BlockingAsync` поверх `Shared`) — порт OTA асинхронный, за ним может
/// стоять и внешняя флеш.
///
/// Тот же цельный `Flash`, что и в `crates-cross/boot`: он сам знает реальные
/// границы секторов чипа, в том числе неравномерные (F4/F7/H7), а банковые
/// регионы у каждого семейства называются по-своему.
{%- if dfu_bank == "true" %}
///
/// Кроме этого чипа: банк 2 у него стоит за разрывом, цельный `Flash` туда не
/// дотягивается, и `DFU` лежит в регионе банка 2 (`crate::board::DfuBankMutex`).
type DfuFlash = BlockingAsync<Shared<'static, DfuBankMutex>>;
{%- elsif swap_paged == "true" %}
///
/// Но со страницей обмена [`SWAP_PAGE_SIZE`], крупнее физической страницы
/// флеша (`adapters::flash::Paged`): `DFU` обязан видеть ту же страницу, что
/// `ACTIVE` и `DFU` в bootloader'е, иначе размеры и смещения разделов у них
/// разойдутся.
type DfuFlash = BlockingAsync<Paged<Shared<'static, FlashMutex>, SWAP_PAGE_SIZE>>;
{%- else %}
type DfuFlash = BlockingAsync<Shared<'static, FlashMutex>>;
{%- endif %}

/// Раздел `DFU` — то, из чего собирается адаптер.
pub type Partition =
    embassy_embedded_hal::flash::partition::Partition<'static, NoopRawMutex, DfuFlash>;

/// Страница раздела `BOOTLOADER_STATE` — размер сектора, в котором он лежит.
///
/// Цельный `Flash` объявляет страницей самый крупный сектор чипа, а
/// `Partition::new` паникует на разделе, не кратном странице. На F2/F4/F7
/// `STATE` лежит в мелком секторе (у F4 — 16 KiB, а с разделом настроек
/// 64 KiB при странице 128 KiB), поэтому его страница своя. Число посчитано при генерации тем же
/// расчётом, что адреса в `memory.x`; то же самое стоит в
/// `crates-cross/boot` — разделу в приложении и в bootloader'е расходиться
/// нельзя.
const STATE_ERASE_SIZE: usize = {{state_erase_size}};
{%- if swap_paged == "true" %}

/// Страница обмена `ACTIVE` и `DFU` — крупнее физической страницы флеша.
///
/// Генерация берёт её больше физической, чтобы журнал `BOOTLOADER_STATE`
/// (запись на каждую страницу `ACTIVE`) стал короче и коду bootloader'а
/// остался почти весь его блок. Раздел `DFU` строится над
/// `Paged<_, SWAP_PAGE_SIZE>`; `ACTIVE` с той же страницей собирает
/// bootloader. Число — `page_size` из генерации, то же самое стоит в
/// `crates-cross/boot` — менять только вместе.
const SWAP_PAGE_SIZE: usize = {{page_size}};

const _: () = assert!(
    SWAP_PAGE_SIZE.is_multiple_of(embassy_stm32::flash::MAX_ERASE_SIZE),
    "page_size из генерации не кратен странице стирания флеша чипа \
     (embassy_stm32::flash::MAX_ERASE_SIZE): стирание раздела заденет чужие секторы"
);
{%- endif %}

/// Флеш под разделом `BOOTLOADER_STATE`: тот же общий `Flash`, но со
/// страницей [`STATE_ERASE_SIZE`].
type StateFlash = BlockingAsync<Paged<Shared<'static, FlashMutex>, STATE_ERASE_SIZE>>;

/// Раздел `BOOTLOADER_STATE` — над [`StateFlash`], а не над цельным `Flash`,
/// как [`Partition`].
pub type StatePartition =
    embassy_embedded_hal::flash::partition::Partition<'static, NoopRawMutex, StateFlash>;

/// Свои асинхронные мьютексы разделам: `embassy-boot` строит каждый раздел
/// из `&Mutex<_, T>` своего типа. Внутри обоих — ссылка на общий
/// [`FlashMutex`], а не второй `Flash`: контроллер на чипе один.
static DFU_FLASH: StaticCell<Mutex<NoopRawMutex, DfuFlash>> = StaticCell::new();
static STATE_FLASH: StaticCell<Mutex<NoopRawMutex, StateFlash>> = StaticCell::new();
{%- endif %}

/// Адаптер обновления этой платы — порт `FirmwareUpdate` поверх разделов
/// `DFU`/`BOOTLOADER_STATE`. Псевдоним по общему правилу шаблона: тип слота
/// узла OTA должен быть конкретным, а адаптер приходит из `adapters` generic.
pub type Updater = adapters::ota::Updater<Partition, StatePartition>;

/// Всё, что плата отдаёт узлу OTA: канал доставки и адаптер разделов.
///
/// Своя структура, а не `domain::ota::OtaWiring`: `bsp` зависит только от
/// `ports` и `adapters`, про `domain` он не знает. Во вход узла её
/// раскладывает compose-site (`crates-cross/app/src/graph.rs`).
#[derive(Debug)]
pub struct Ota {
    /// Канал доставки образа — реализация `ports::ImageSource`.
    pub link: Link,
    /// Адаптер разделов — реализация `ports::FirmwareUpdate`.
    pub flash: Updater,
}

/// Канал доставки образа этой платы — заглушка, которую проект заменяет
/// своим транспортом.
///
/// Каркас, как `domain::heartbeat::run`: узел `OTA` уже собран и спавнится{% if graph == "true" %} графом{% else %} из `main`{% endif %},
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
#[derive(Debug)]
pub struct Link;

impl ImageSource for Link {
    type Error = Infallible;

    async fn begin(&mut self) -> Result<Announce, Self::Error> {
        defmt::warn!("bsp: транспорт OTA не реализован — узел OTA ждёт впустую");
        core::future::pending().await
    }

    fn next(&mut self) -> impl Future<Output = Result<Option<&[u8]>, Self::Error>> {
        core::future::ready(Ok(None))
    }

    fn finish(
        &mut self,
        _outcome: Result<(), Rejection>,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Ok(()))
    }
}

/// Собирает вход узла OTA: адаптер из разделов `DFU` и `BOOTLOADER_STATE`,
/// найденных по символам `memory.x`, и канал доставки.
{%- if ota_store == "external" %}
///
/// Оба раздела — над одной внешней флешью: символы в `memory.x` для них —
/// смещения от её начала.
pub(crate) fn new(external: &'static ExternalMutex) -> Ota {
    let config = FirmwareUpdaterConfig::from_linkerfile(external, external);
{%- else %}
{%- if dfu_bank == "true" %}
///
/// `DFU` — в банке 2 (`dfu_bank`), `STATE` — в банке 1 (`flash`).
pub(crate) fn new(flash: &'static FlashMutex, dfu_bank: &'static DfuBankMutex) -> Ota {
    let dfu = DFU_FLASH.init(Mutex::new(BlockingAsync::new(Shared::new(dfu_bank))));
{%- elsif swap_paged == "true" %}
pub(crate) fn new(flash: &'static FlashMutex) -> Ota {
    let dfu = BlockingAsync::new(Paged::new(Shared::new(flash)));
    let dfu = DFU_FLASH.init(Mutex::new(dfu));
{%- else %}
pub(crate) fn new(flash: &'static FlashMutex) -> Ota {
    let dfu = DFU_FLASH.init(Mutex::new(BlockingAsync::new(Shared::new(flash))));
{%- endif %}
    let state = BlockingAsync::new(Paged::new(Shared::new(flash)));
    let state = STATE_FLASH.init(Mutex::new(state));
    let config = FirmwareUpdaterConfig::from_linkerfile(dfu, state);
{%- endif %}
    Ota {
        link: Link,
        flash: Updater::{% if signed == "true" %}signed{% else %}new{% endif %}(config.dfu, config.state, max_image_len()),
    }
}

// Одно из `#[expect(unsafe_code)]` в `crates-cross` (остальные —
// `boot/src/main.rs` и `app/src/main.rs`): взять
// адрес линкерного символа иначе нельзя, а сам символ — это ровно то, для чего
// `extern` и существует. `clippy::undocumented_unsafe_blocks` этот блок не видит
// — он про `unsafe`-блоки, а не про объявления, — поэтому объяснение здесь.
#[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
unsafe extern "C" {
    /// Границы раздела `ACTIVE` — те же символы, по которым разделы находит
    /// `FirmwareUpdaterConfig::from_linkerfile`.
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
