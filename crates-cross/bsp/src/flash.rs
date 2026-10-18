#![doc = include_str!("../../../docs/modules/bsp-flash.md")]
{%- if ota == "true" and ota_store != "external" %}
use adapters::flash::{ErasePage, whole};
{%- endif %}
{%- if shared_flash == "true" %}
use core::cell::RefCell;
{%- endif %}
{%- if ota == "true" %}
use embassy_boot::FirmwareUpdaterConfig;
{%- endif %}
{%- if ota == "true" and ota_store != "external" %}
use embassy_embedded_hal::adapter::BlockingAsync;
{%- endif %}
{%- if shared_flash == "true" %}
use embassy_embedded_hal::flash::partition::BlockingPartition;
{%- if dfu_bank == "true" %}
use embassy_stm32::flash::{Bank1Region, Bank2Region};
{%- endif %}
use embassy_stm32::flash::{Blocking, Flash};
use embassy_stm32::{Peri, peripherals};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
{%- endif %}
{%- if ota == "true" and ota_store != "external" %}
use embassy_sync::mutex::Mutex as AsyncMutex;
{%- endif %}
{%- if shared_flash == "true" %}
use static_cell::StaticCell;
{%- endif %}
{%- if ota == "true" and ota_store == "external" %}

use crate::external::{ExternalMutex, ExternalPartition};
{%- endif %}
{%- if shared_flash == "true" %}

/// Общий `Flash` всего чипа под блокирующим замком. Контроллер флеша один, а
/// работать с ним нужно и OTA (разделы `DFU`/`BOOTLOADER_STATE`), и настройкам
/// (раздел `CONFIG` во флеше или внутренняя EEPROM — она пишется через тот же
/// контроллер), — поэтому объект создаётся ровно один и раздаётся ссылками;
/// к нему обращается [`Whole`] — раздел `embassy-embedded-hal` на весь флеш
/// (`adapters::flash::whole`).
///
/// `NoopRawMutex`, а не `CriticalSectionRawMutex`: обе половины работают из
/// задач одного исполнителя, то есть без вытеснения. Появится второй
/// исполнитель (прерывательный приоритет, второе ядро) — менять здесь.
pub type FlashMutex = Mutex<NoopRawMutex, RefCell<ChipFlash>>;
{%- if dfu_bank == "true" %}

/// Флеш под [`FlashMutex`] — банк 1, а не весь чип: банк 2 стоит за
/// разрывом, и цельный `Flash` туда не дотягивается (`FLASH_SIZE` у него —
/// сумма банков, меньше смещения банка 2). Смещения в банке 1 те же, что у
/// цельного `Flash`, — от базы flash.
pub type ChipFlash = Bank1Region<'static, Blocking>;

/// Банк 2 — под `DFU`, и только под него: смещения в нём отсчитаны от его
/// начала, и символы `memory.x` у `DFU` посчитаны так же.
pub type DfuBankMutex = Mutex<NoopRawMutex, RefCell<Bank2Region<'static, Blocking>>>;

/// Весь банк 2 как раздел под замком — под него строится `DFU`.
type DfuBank = BlockingPartition<'static, NoopRawMutex, Bank2Region<'static, Blocking>>;

/// `&'static` — по той же причине, что [`FLASH`].
static DFU_BANK: StaticCell<DfuBankMutex> = StaticCell::new();
{%- else %}

/// Флеш под [`FlashMutex`] — весь чип одним объектом.
pub type ChipFlash = Flash<'static, Blocking>;
{%- endif %}

/// Весь флеш как раздел под замком: из него строятся разделы OTA и настроек.
pub type Whole = BlockingPartition<'static, NoopRawMutex, ChipFlash>;
{%- if dfu_bank != "true" %}

// `BlockingPartition::new` паникует на объёме, не кратном `ERASE_SIZE`; у STM32
// объём всегда кратен самому крупному сектору, и сборка это проверяет, а не
// первый запуск на плате.
const _: () = assert!(
    embassy_stm32::flash::FLASH_SIZE.is_multiple_of(embassy_stm32::flash::MAX_ERASE_SIZE),
    "объём flash не кратен самому крупному сектору: `adapters::flash::whole` паникует на старте"
);
{%- endif %}

/// `&'static` он не для красоты: разделы OTA и `Settings` отдаются в задачи, а
/// аргументы задач embassy обязаны быть `'static`. Обычное поле `Board` этого
/// не даёт — `Board` живёт в `main`, а не вечно.
static FLASH: StaticCell<FlashMutex> = StaticCell::new();

/// Флеш чипа, как его видит `Board::new`: общий замок{% if dfu_bank == "true" %} и банк 2 под `DFU`{% endif %}.
#[derive(Clone, Copy)]
pub(crate) struct Chip {
    pub(crate) shared: &'static FlashMutex,
{%- if dfu_bank == "true" %}
    pub(crate) dfu_bank: &'static DfuBankMutex,
{%- endif %}
}

/// Забирает контроллер флеша и заводит общий замок. Зовётся один раз.
pub(crate) fn chip(flash: Peri<'static, peripherals::FLASH>) -> Chip {
{%- if dfu_bank == "true" %}
    let banks = Flash::new_blocking(flash).into_blocking_regions();
    Chip {
        shared: FLASH.init(Mutex::new(RefCell::new(banks.bank1_region))),
        dfu_bank: DFU_BANK.init(Mutex::new(RefCell::new(banks.bank2_region))),
    }
{%- else %}
    Chip {
        shared: FLASH.init(Mutex::new(RefCell::new(Flash::new_blocking(flash)))),
    }
{%- endif %}
}
{%- endif %}
{%- if ota == "true" %}
{%- if ota_store != "external" %}

/// Флеш под разделом `DFU`: общий `Flash` чипа, видимый асинхронно
/// (`BlockingAsync` поверх [`Whole`]) — порт OTA асинхронный, за ним может
/// стоять и внешняя флеш.
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
type DfuFlash = BlockingAsync<ErasePage<Whole, SWAP_PAGE_SIZE>>;
{%- else %}
type DfuFlash = BlockingAsync<Whole>;
{%- endif %}

/// Страница раздела `BOOTLOADER_STATE` — размер сектора, в котором он лежит.
///
/// Цельный `Flash` объявляет страницей самый крупный сектор чипа, а
/// `Partition::new` паникует на разделе, не кратном странице. На F2/F4/F7
/// `STATE` лежит в мелком секторе (у F4 — 16 KiB, а с разделом настроек
/// 64 KiB при странице 128 KiB), поэтому его страница своя. Число посчитано при
/// генерации тем же расчётом, что адреса в `memory.x`, и приезжает переменной
/// среды `STATE_ERASE_SIZE` из `crates-cross/.cargo/config.toml` — её же читает
/// bootloader, разделу в приложении и в нём расходиться нельзя. Нет переменной —
/// страница цельного `Flash`.
const STATE_ERASE_SIZE: usize = adapters::flash::size_from_env(
    option_env!("STATE_ERASE_SIZE"),
    embassy_stm32::flash::MAX_ERASE_SIZE,
);
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
const SWAP_PAGE_SIZE: usize = adapters::flash::size_from_env(option_env!("SWAP_PAGE_SIZE"), 0);

const _: () = assert!(
    SWAP_PAGE_SIZE != 0 && SWAP_PAGE_SIZE.is_multiple_of(embassy_stm32::flash::MAX_ERASE_SIZE),
    "SWAP_PAGE_SIZE из генерации не задан или не кратен странице стирания флеша чипа \
     (embassy_stm32::flash::MAX_ERASE_SIZE): стирание раздела заденет чужие секторы"
);
{%- endif %}

/// Флеш под разделом `BOOTLOADER_STATE`: тот же общий `Flash`, но со
/// страницей [`STATE_ERASE_SIZE`].
type StateFlash = BlockingAsync<ErasePage<Whole, STATE_ERASE_SIZE>>;

/// Раздел `DFU` — то, из чего собирается адаптер обновления.
pub type Dfu = embassy_embedded_hal::flash::partition::Partition<'static, NoopRawMutex, DfuFlash>;

/// Раздел `BOOTLOADER_STATE` — над [`StateFlash`], а не над цельным `Flash`,
/// как [`Dfu`].
pub type State =
    embassy_embedded_hal::flash::partition::Partition<'static, NoopRawMutex, StateFlash>;

/// Свои асинхронные мьютексы разделам: `embassy-boot` строит каждый раздел
/// из `&Mutex<_, T>` своего типа. Внутри обоих — ссылка на общий
/// [`FlashMutex`], а не второй `Flash`: контроллер на чипе один.
static DFU_FLASH: StaticCell<AsyncMutex<NoopRawMutex, DfuFlash>> = StaticCell::new();
static STATE_FLASH: StaticCell<AsyncMutex<NoopRawMutex, StateFlash>> = StaticCell::new();
{%- else %}

/// Раздел `DFU` — на внешней флеши (`crate::external`).
pub type Dfu = ExternalPartition;

/// Раздел `BOOTLOADER_STATE` — на той же внешней флеши: его страницу несёт
/// драйвер.
pub type State = ExternalPartition;
{%- endif %}

/// Разделы обновления прошивки — то, что плата знает о раскладке флеша:
/// `DFU`, `BOOTLOADER_STATE` и размер `ACTIVE`. Алгоритм обновления над ними
/// (адаптер `adapters::ota::Updater` и узел `domain::ota`) собирает `app`.
pub struct Partitions {
    /// Раздел, куда пишется новый образ.
    pub dfu: Dfu,
    /// Раздел состояния bootloader'а.
    pub state: State,
    /// Самый длинный образ, который доедет до устройства, — размер `ACTIVE`,
    /// а НЕ `DFU`, хотя принимается образ в `DFU`. Раздел обновления по
    /// построению больше активного на одну-две страницы (этого требует
    /// `assert_partitions` в `embassy-boot`: обмену нужна запасная страница),
    /// а обмен копирует ровно `ACTIVE`. Образ размером между ними прошёл бы
    /// и проверку длины, и подпись, и пометку к обмену — а в `ACTIVE`
    /// приехал бы обрезанным: устройство отработало бы полный цикл обновления
    /// с перезагрузкой и молча откатилось.
    pub active_len: u32,
}

// Вручную: у разделов `embassy-embedded-hal` нет `Debug`.
impl core::fmt::Debug for Partitions {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Partitions")
            .field("active_len", &self.active_len)
            .finish_non_exhaustive()
    }
}

/// Собирает разделы `DFU` и `BOOTLOADER_STATE`, найденные по символам
/// `memory.x`.
{%- if ota_store == "external" %}
///
/// Оба раздела — над одной внешней флешью: символы в `memory.x` для них —
/// смещения от её начала.
pub(crate) fn partitions(external: &'static ExternalMutex) -> Partitions {
    let config = FirmwareUpdaterConfig::from_linkerfile(external, external);
{%- else %}
{%- if dfu_bank == "true" %}
///
/// `DFU` — в банке 2 (`dfu_bank`), `STATE` — в банке 1 (`shared`).
pub(crate) fn partitions(chip: Chip) -> Partitions {
    let dfu = DFU_FLASH.init(AsyncMutex::new(BlockingAsync::new(whole(chip.dfu_bank))));
{%- elsif swap_paged == "true" %}
pub(crate) fn partitions(chip: Chip) -> Partitions {
    let dfu = BlockingAsync::new(ErasePage::new(whole(chip.shared)));
    let dfu = DFU_FLASH.init(AsyncMutex::new(dfu));
{%- else %}
pub(crate) fn partitions(chip: Chip) -> Partitions {
    let dfu = DFU_FLASH.init(AsyncMutex::new(BlockingAsync::new(whole(chip.shared))));
{%- endif %}
    let state = BlockingAsync::new(ErasePage::new(whole(chip.shared)));
    let state = STATE_FLASH.init(AsyncMutex::new(state));
    let config = FirmwareUpdaterConfig::from_linkerfile(dfu, state);
{%- endif %}
    Partitions {
        dfu: config.dfu,
        state: config.state,
        active_len: active_len(),
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

/// Размер `ACTIVE` — см. [`Partitions::active_len`].
///
/// Функция, а не `const`: это разность адресов линкерных символов, а их
/// значения известны линкеру, а не компилятору.
fn active_len() -> u32 {
    // SAFETY: символы объявлены линкерным скриптом как абсолютные значения;
    // берётся их адрес, а не содержимое.
    let start = &raw const __bootloader_active_start as u32;
    let end = &raw const __bootloader_active_end as u32;
    end.saturating_sub(start)
}
{%- endif %}
