#![doc = include_str!("../../../docs/modules/bsp-ota.md")]

{%- if ota_store == "external" %}
use embassy_boot::FirmwareUpdaterConfig;

use crate::external::{ExternalMutex, ExternalPartition};
{%- else %}
{%- if state_paged == "true" or swap_paged == "true" and dfu_bank != "true" %}
use adapters::flash::ErasePage;
{%- endif %}
use embassy_boot::FirmwareUpdaterConfig;
use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_embedded_hal::flash::partition::BlockingPartition;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use embassy_sync::mutex::Mutex;
use static_cell::StaticCell;

{%- if dfu_bank == "true" %}

use crate::flash::{DFU_BANK_SIZE, DfuBank, DfuBankMutex, FlashMutex, SIZE};
{%- else %}

use crate::flash::{FlashMutex, SIZE};
{%- endif %}
{%- endif %}

/// Размер `ACTIVE` в байтах: самый длинный образ, который доедет до
/// устройства. Это `ACTIVE`, а НЕ `DFU`, хотя принимается образ в `DFU`.
/// Раздел обновления по построению больше активного на одну-две страницы (этого
/// требует `assert_partitions` в `embassy-boot`: обмену нужна запасная
/// страница), а обмен копирует ровно `ACTIVE`. Образ размером между ними прошёл
/// бы и проверку длины, и подпись, и пометку к обмену — а в `ACTIVE` приехал бы
/// обрезанным: устройство отработало бы полный цикл обновления с перезагрузкой и
/// молча откатилось.
///
/// Число — переменная среды `ACTIVE_LEN` из `[env]`
/// `crates-cross/.cargo/config.toml`: тем же расчётом генерация пишет `ACTIVE` в
/// `memory.x`, и читать размер из линкерных символов (`unsafe extern`) незачем.
const ACTIVE_LEN: u32 = match u32::from_str_radix(env!("ACTIVE_LEN"), 10) {
    Ok(len) => len,
    Err(_) => panic!("ACTIVE_LEN в [env] crates-cross/.cargo/config.toml — не число"),
};
{%- if ota_store != "external" %}

/// Страница раздела `BOOTLOADER_STATE` — размер сектора, в котором он лежит.
///
/// Цельный `Flash` объявляет страницей самый крупный сектор чипа, а
/// `Partition::new` паникует на разделе, не кратном странице. На F2/F4/F7
/// `STATE` лежит в мелком секторе (у F4 — 16 KiB, а с разделом настроек
/// 64 KiB при странице 128 KiB), поэтому его страница своя{% if state_paged != "true" %}; у этого чипа
/// она равна странице цельного `Flash`, и раздел строится прямо над ним, а
/// равенство держит `const`-assert ниже{% endif %}. Число посчитано при
/// генерации тем же расчётом, что адреса в `memory.x`, и приезжает переменной
/// среды `STATE_ERASE_SIZE` из `[env]` `crates-cross/.cargo/config.toml` — её же
/// читает bootloader (он от `bsp` не зависит), и разделу в приложении и в нём
/// расходиться нельзя.
const STATE_ERASE_SIZE: usize = match usize::from_str_radix(env!("STATE_ERASE_SIZE"), 10) {
    Ok(size) => size,
    Err(_) => panic!("STATE_ERASE_SIZE в [env] crates-cross/.cargo/config.toml — не число"),
};
{%- if state_paged != "true" %}

const _: () = assert!(
    STATE_ERASE_SIZE == embassy_stm32::flash::MAX_ERASE_SIZE,
    "STATE_ERASE_SIZE из генерации не равен странице цельного `Flash` \
     (embassy_stm32::flash::MAX_ERASE_SIZE): `STATE` нужна своя страница"
);
{%- endif %}
{%- if swap_paged == "true" %}

/// Страница обмена `ACTIVE` и `DFU` — крупнее физической страницы флеша.
///
/// Генерация берёт её больше физической, чтобы журнал `BOOTLOADER_STATE`
/// (запись на каждую страницу `ACTIVE`) стал короче и коду bootloader'а
/// остался почти весь его блок. Раздел `DFU` строится над
/// `ErasePage<_, SWAP_PAGE_SIZE>`; `ACTIVE` с той же страницей собирает
/// bootloader. Число — `page_size` из генерации, переменная среды
/// `SWAP_PAGE_SIZE` (общая с bootloader'ом); кратно оно странице стирания
/// флеша (проверка ниже).
const SWAP_PAGE_SIZE: usize = match usize::from_str_radix(env!("SWAP_PAGE_SIZE"), 10) {
    Ok(size) => size,
    Err(_) => panic!("SWAP_PAGE_SIZE в [env] crates-cross/.cargo/config.toml — не число"),
};

const _: () = assert!(
    SWAP_PAGE_SIZE.is_multiple_of(embassy_stm32::flash::MAX_ERASE_SIZE),
    "SWAP_PAGE_SIZE из генерации не кратен странице стирания флеша чипа \
     (embassy_stm32::flash::MAX_ERASE_SIZE): стирание раздела заденет чужие секторы"
);
{%- endif %}

/// Свои асинхронные мьютексы разделам: `embassy-boot` строит каждый раздел
/// из `&Mutex<_, T>` своего типа. Внутри обоих — ссылка на общий
/// [`FlashMutex`], а не второй `Flash`: контроллер на чипе один.
static DFU_FLASH: StaticCell<Mutex<NoopRawMutex, DfuFlash>> = StaticCell::new();
static STATE_FLASH: StaticCell<Mutex<NoopRawMutex, StateFlash>> = StaticCell::new();

/// Флеш под разделом `DFU`: общий `Flash` чипа, видимый асинхронно
/// (`BlockingAsync`) — порт OTA асинхронный, за ним может стоять и внешняя
/// флеш.
///
/// Тот же цельный `Flash`, что и в `crates-cross/boot`: он сам знает реальные
/// границы секторов чипа, в том числе неравномерные (F4/F7/H7), а банковые
/// регионы у каждого семейства называются по-своему.
{%- if dfu_bank == "true" %}
///
/// Кроме этого чипа: банк 2 у него стоит за разрывом, цельный `Flash` туда не
/// дотягивается, и `DFU` лежит в регионе банка 2 ([`DfuBankMutex`]).
type DfuFlash = BlockingAsync<DfuBank>;
{%- elsif swap_paged == "true" %}
///
/// Но со страницей обмена [`SWAP_PAGE_SIZE`], крупнее физической страницы
/// флеша (`adapters::flash::ErasePage`): `DFU` обязан видеть ту же страницу, что
/// `ACTIVE` и `DFU` в bootloader'е, иначе размеры и смещения разделов у них
/// разойдутся.
type DfuFlash = BlockingAsync<ErasePage<crate::flash::Whole, SWAP_PAGE_SIZE>>;
{%- else %}
type DfuFlash = BlockingAsync<crate::flash::Whole>;
{%- endif %}

{%- if state_paged == "true" %}
/// Флеш под разделом `BOOTLOADER_STATE`: тот же общий `Flash`, но со
/// страницей [`STATE_ERASE_SIZE`].
type StateFlash = BlockingAsync<ErasePage<crate::flash::Whole, STATE_ERASE_SIZE>>;
{%- else %}
/// Флеш под разделом `BOOTLOADER_STATE`: тот же общий `Flash` — страница сектора
/// `STATE` совпадает с его страницей.
type StateFlash = BlockingAsync<crate::flash::Whole>;
{%- endif %}

/// Раздел `DFU` — то, из чего собирается адаптер обновления.
type Dfu = embassy_embedded_hal::flash::partition::Partition<'static, NoopRawMutex, DfuFlash>;

/// Раздел `BOOTLOADER_STATE` — над [`StateFlash`], а не над цельным `Flash`,
/// как [`Dfu`].
type State = embassy_embedded_hal::flash::partition::Partition<'static, NoopRawMutex, StateFlash>;
{%- else %}

/// Раздел `DFU` — на внешней флеши (`crate::external`).
type Dfu = ExternalPartition;

/// Раздел `BOOTLOADER_STATE` — на той же внешней флеши: его страницу несёт
/// драйвер.
type State = ExternalPartition;
{%- endif %}

/// Канал доставки образа этой платы — заглушка, которую проект заменяет
/// своим транспортом (как реализовать — `adapters::ota::Unconnected`).
///
/// Псевдоним, а не голый тип в графе и в задаче: конкретный тип слота приходит
/// под именем, и подставить свой транспорт — значит поменять эту строку и
/// [`link`].
pub type Link = adapters::ota::Unconnected;

/// Адаптер обновления этой платы — порт `FirmwareUpdate` поверх разделов
/// `DFU`/`BOOTLOADER_STATE`. Псевдоним по общему правилу шаблона: тип слота
/// узла OTA должен быть конкретным, а адаптер приходит из `adapters` generic.
pub type Updater = adapters::ota::Updater<Dfu, State, {{signed}}>;

/// Канал доставки образа. Пока в слоте заглушка, об этом говорит одна строка
/// в лог при старте.
pub(crate) fn link() -> Link {
    defmt::warn!("bsp: транспорт OTA не реализован — узел OTA ждёт впустую");
    Link::default()
}

/// Собирает адаптер обновления из разделов `DFU` и `BOOTLOADER_STATE`,
/// найденных по символам `memory.x`.
{%- if ota_store == "external" %}
///
/// Оба раздела — над одной внешней флешью: символы в `memory.x` для них —
/// смещения от её начала.
pub(crate) fn updater(external: &'static ExternalMutex) -> Updater {
    let config = FirmwareUpdaterConfig::from_linkerfile(external, external);
{%- else %}
{%- if dfu_bank == "true" %}
///
/// `DFU` — в банке 2, `STATE` — в банке 1.
pub(crate) fn updater(flash: &'static FlashMutex, dfu_bank: &'static DfuBankMutex) -> Updater {
    let dfu = BlockingAsync::new(BlockingPartition::new(dfu_bank, 0, DFU_BANK_SIZE));
{%- elsif swap_paged == "true" %}
pub(crate) fn updater(flash: &'static FlashMutex) -> Updater {
    let dfu = BlockingAsync::new(ErasePage::new(BlockingPartition::new(flash, 0, SIZE)));
{%- else %}
pub(crate) fn updater(flash: &'static FlashMutex) -> Updater {
    let dfu = BlockingAsync::new(BlockingPartition::new(flash, 0, SIZE));
{%- endif %}
    let dfu = DFU_FLASH.init(Mutex::new(dfu));
{%- if state_paged == "true" %}
    let state = BlockingAsync::new(ErasePage::new(BlockingPartition::new(flash, 0, SIZE)));
{%- else %}
    let state = BlockingAsync::new(BlockingPartition::new(flash, 0, SIZE));
{%- endif %}
    let state = STATE_FLASH.init(Mutex::new(state));
    let config = FirmwareUpdaterConfig::from_linkerfile(dfu, state);
{%- endif %}
    Updater::new(config.dfu, config.state, ACTIVE_LEN)
}
