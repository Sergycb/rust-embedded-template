#![doc = include_str!("../../../docs/modules/bsp-flash.md")]
{%- assign firmware_here = "false" %}
{%- if ota == "true" %}
{%- if ota_store != "external" %}
{%- assign firmware_here = "true" %}
{%- endif %}
{%- endif %}
{%- assign config_here = "false" %}
{%- if config == "true" %}
{%- if config_store == "flash" %}
{%- assign config_here = "true" %}
{%- endif %}
{%- endif %}
{%- assign eeprom_here = "false" %}
{%- if config == "true" %}
{%- if config_store == "eeprom" %}
{%- assign eeprom_here = "true" %}
{%- endif %}
{%- endif %}
{%- assign whole = "false" %}
{%- if firmware_here == "true" or config_here == "true" %}
{%- assign whole = "true" %}
{%- endif %}
{%- assign paged = "false" %}
{%- if firmware_here == "true" %}
{%- if state_paged == "true" %}
{%- assign paged = "true" %}
{%- endif %}
{%- if swap_paged == "true" %}
{%- if dfu_bank != "true" %}
{%- assign paged = "true" %}
{%- endif %}
{%- endif %}
{%- endif %}
{%- if config_here == "true" %}
{%- if config_paged == "true" %}
{%- assign paged = "true" %}
{%- endif %}
{%- endif %}
{%- assign flash_cache = "false" %}
{%- if cache contains "scb_d" or cache contains "icache" or cache contains "dcen" %}
{%- assign flash_cache = "true" %}
{%- endif %}

use core::cell::RefCell;
{% if whole == "true" %}
use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_embedded_hal::flash::partition::BlockingPartition;
{%- endif %}
{%- if config_here == "true" or firmware_here == "true" %}
use embassy_embedded_hal::flash::partition::Partition;
{%- endif %}
{%- if dfu_bank == "true" %}
use embassy_stm32::flash::{BANK1_REGION, BANK2_REGION, Bank1Region, Bank2Region, Blocking};
{%- elsif whole == "true" %}
use embassy_stm32::flash::{Blocking, FLASH_SIZE, MAX_ERASE_SIZE};
{%- else %}
use embassy_stm32::flash::Blocking;
{%- endif %}
{%- if whole != "true" %}
use embassy_sync::blocking_mutex;
{%- endif %}
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
{%- if whole == "true" %}
use embassy_sync::{blocking_mutex, mutex};
{%- endif %}
{%- if flash_cache == "true" %}
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
{%- endif %}
{%- if eeprom_here == "true" %}
use embedded_storage::{ReadStorage, Storage};
{%- endif %}
{%- if paged == "true" %}
use flash_page::ErasePage;
{%- endif %}
use static_cell::StaticCell;
{%- if firmware_here == "true" %}

/// Страница раздела `BOOTLOADER_STATE` — размер сектора, в котором он лежит.
///
/// Своя у `STATE`, потому что `Partition::new` паникует на разделе, не кратном
/// странице цельного `Flash`{% if state_paged != "true" %}; у этого чипа она равна странице цельного
/// `Flash`, и равенство держит `const`-assert ниже{% endif %}. Число — из генерации
/// (`STATE_ERASE_SIZE` в `[env]`), его же берёт отсюда bootloader
/// (docs/flash.md, «Страница раздела — {% if erase_page == "true" %}не всегда {% endif %}страница чипа»).
pub const STATE_ERASE_SIZE: usize = match usize::from_str_radix(env!("STATE_ERASE_SIZE"), 10) {
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
/// Крупнее — ради короткого журнала `BOOTLOADER_STATE` (docs/flash.md,
/// «Страница раздела — не всегда страница чипа»). Флеш под `DFU` —
/// `ErasePage<_, SWAP_PAGE_SIZE>`; `ACTIVE` с той же страницей собирает
/// bootloader (отсюда же берёт и число). Число — `page_size` из генерации,
/// переменная среды `SWAP_PAGE_SIZE`; кратно оно странице стирания флеша
/// (проверка ниже).
pub const SWAP_PAGE_SIZE: usize = match usize::from_str_radix(env!("SWAP_PAGE_SIZE"), 10) {
    Ok(size) => size,
    Err(_) => panic!("SWAP_PAGE_SIZE в [env] crates-cross/.cargo/config.toml — не число"),
};

const _: () = assert!(
    SWAP_PAGE_SIZE.is_multiple_of(embassy_stm32::flash::MAX_ERASE_SIZE),
    "SWAP_PAGE_SIZE из генерации не кратен странице стирания флеша чипа \
     (embassy_stm32::flash::MAX_ERASE_SIZE): стирание раздела заденет чужие секторы"
);
{%- endif %}
{%- endif %}
{%- if whole == "true" %}
{%- if dfu_bank != "true" %}

/// Размер всего флеша в байтах — столько занимает раздел [`Whole`].
///
/// `usize` на Cortex-M — 32 бита, усечения нет.
#[expect(
    clippy::cast_possible_truncation,
    reason = "`usize` на Cortex-M — 32 бита"
)]
const SIZE: u32 = FLASH_SIZE as u32;

// `BlockingPartition::new` паникует на объёме, не кратном `ERASE_SIZE`; у STM32
// объём всегда кратен самому крупному сектору, и сборка это проверяет, а не
// первый запуск на плате.
const _: () = assert!(
    FLASH_SIZE.is_multiple_of(MAX_ERASE_SIZE),
    "объём flash не кратен самому крупному сектору: `BlockingPartition::new` паникует на старте"
);
{%- else %}

/// Размер банка 1 в байтах — столько занимает раздел [`Whole`]. Банк, а не
/// весь чип: банк 2 стоит за разрывом, и цельный `Flash` туда не дотягивается
/// (`FLASH_SIZE` у него — сумма банков, меньше смещения банка 2).
const SIZE: u32 = BANK1_REGION.size;

/// Размер банка 2 в байтах — столько занимает раздел под `DFU`.
const DFU_BANK_SIZE: u32 = BANK2_REGION.size;
{%- endif %}
{%- endif %}
{%- if config_here == "true" %}

/// Страница раздела `CONFIG` — размер сектора, в котором он лежит.
///
/// `sequential-storage` делит раздел на страницы по `ERASE_SIZE` флеша, а
/// цельный `Flash` объявляет им самый крупный сектор чипа. Раздел в мелких
/// секторах (F2/F4/F7{% if ota == "true" %} с OTA{% endif %}) со страницей всего чипа не делится на две, поэтому
/// страница своя{% if config_paged == "true" %} (`ErasePage`){% endif %}; в хвосте flash она совпадает со страницей чипа{% if config_paged != "true" %} — как у
/// этого чипа: раздел строится прямо над [`Whole`], а равенство держит
/// `const`-assert ниже{% endif %}.
/// Число посчитано при генерации тем же расчётом, что адреса в `memory.x`; тем же
/// литералом там стоит `ASSERT` — границы `CONFIG` кратны ей, иначе
/// `Partition::new` паниковал бы на старте, а так падает линковка.
const CONFIG_ERASE_SIZE: usize = {{config_erase_size}};
{%- if config_paged != "true" %}

const _: () = assert!(
    CONFIG_ERASE_SIZE == embassy_stm32::flash::MAX_ERASE_SIZE,
    "CONFIG_ERASE_SIZE из генерации не равен странице цельного `Flash` \
     (embassy_stm32::flash::MAX_ERASE_SIZE): `CONFIG` нужна своя страница"
);
{%- endif %}

{%- endif %}
{%- if firmware_here == "true" and config_here == "true" %}

/// Общий `Flash` всего чипа под блокирующим замком. Контроллер флеша один, а
/// работать с ним нужно и обновлению (флеш под `DFU`/`BOOTLOADER_STATE`), и
/// настройкам (раздел `CONFIG`), — поэтому объект создаётся ровно один и
/// раздаётся ссылками.
{%- elsif firmware_here == "true" and eeprom_here == "true" %}

/// Общий `Flash` всего чипа под блокирующим замком. Контроллер флеша один, а
/// работать с ним нужно и обновлению (флеш под `DFU`/`BOOTLOADER_STATE`), и
/// настройкам (внутренняя EEPROM — она пишется через тот же контроллер), —
/// поэтому объект создаётся ровно один и раздаётся ссылками.
{%- elsif firmware_here == "true" %}

/// Общий `Flash` всего чипа под блокирующим замком: флеш под
/// `DFU`/`BOOTLOADER_STATE`. Объект создаётся ровно один и раздаётся ссылками.
{%- elsif eeprom_here == "true" %}

/// Общий `Flash` всего чипа под блокирующим замком: через него пишется
/// внутренняя EEPROM настроек. Объект создаётся ровно один и раздаётся ссылками.
{%- else %}

/// Общий `Flash` всего чипа под блокирующим замком: раздел `CONFIG` настроек.
/// Объект создаётся ровно один и раздаётся ссылками.
{%- endif %}
///
/// `NoopRawMutex`, а не `CriticalSectionRawMutex`: все пользователи работают из
/// задач одного исполнителя; что менять вместе с ним — docs/flash.md.
pub type FlashMutex = blocking_mutex::Mutex<NoopRawMutex, RefCell<ChipFlash>>;
{%- if dfu_bank == "true" %}

/// Флеш под [`FlashMutex`] — банк 1, а не весь чип. Смещения в банке 1 те же,
/// что у цельного `Flash`, — от базы flash.
pub type ChipFlash = {% if flash_cache == "true" %}Coherent<Bank1Region<'static, Blocking>>{% else %}Bank1Region<'static, Blocking>{% endif %};

/// Банк 2 — под `DFU`, и только под него: смещения в нём отсчитаны от его
/// начала, и символы `memory.x` у `DFU` посчитаны так же.
pub type DfuBankMutex = blocking_mutex::Mutex<NoopRawMutex, RefCell<DfuBankFlash>>;

/// Банк 2 как флеш{% if flash_cache == "true" %} — с той же обёрткой кэша, что [`ChipFlash`]{% endif %}.
pub type DfuBankFlash = {% if flash_cache == "true" %}Coherent<Bank2Region<'static, Blocking>>{% else %}Bank2Region<'static, Blocking>{% endif %};
{%- else %}

/// Флеш под [`FlashMutex`] — весь чип одним объектом.
pub type ChipFlash = {% if flash_cache == "true" %}Coherent<embassy_stm32::flash::Flash<'static, Blocking>>{% else %}embassy_stm32::flash::Flash<'static, Blocking>{% endif %};
{%- endif %}
{%- if firmware_here == "true" %}

/// Разделы обновления — рычаг под адаптером обновления
/// (`adapters::ota::Updater`): `DFU` и `BOOTLOADER_STATE`,
/// нарезанные платой по символам `memory.x` (`crate::layout`), со своими
/// страницами, размер `ACTIVE` и отпечаток раскладки.
pub type FirmwareFlash = adapters::ota::Partitions<
    Partition<'static, NoopRawMutex, DfuFlash>,
    Partition<'static, NoopRawMutex, StateFlash>,
>;
{%- endif %}
{%- if config_here == "true" %}

/// Раздел `CONFIG` — носитель хранилища настроек (`adapters::settings`).
/// Именно раздел, а не весь
/// флеш: запись за его границы отказывает, а не стирает {% if ota == "true" %}`ACTIVE` или bootloader{% else %}прошивку{% endif %}.
pub type ConfigFlash = Partition<'static, NoopRawMutex, ConfigBacking>;
{%- endif %}
{%- if eeprom_here == "true" %}

/// Внутренняя EEPROM как `NorFlash` — носитель настроек, над которым
/// стоит хранилище (`adapters::settings`).
/// Флеш под настройки не тратится вовсе: EEPROM пишется через тот же
/// контроллер, что и флеш, поэтому доступ к ней — через общий [`FlashMutex`]
/// (`embassy_stm32::flash::Flash::eeprom_*`), по операции под замком. Обёртку
/// `adapters::eeprom::Eeprom` (инверсия байтов, страница) собирает плата, и
/// под хранилище отдана вся EEPROM, округлённая до её страницы.
pub type ConfigFlash = adapters::eeprom::Eeprom<EepromFlash>;
{%- endif %}
{%- if dfu_bank == "true" %}

/// Весь банк 2 как раздел под замком — под него строится `DFU`.
type DfuBank = BlockingPartition<'static, NoopRawMutex, DfuBankFlash>;
{%- endif %}
{%- if whole == "true" %}

/// Весь флеш как один раздел: смещения те же, что у самого флеша, а операции
/// идут под замком, по одной. Из него строятся носители {% if firmware_here == "true" and config_here == "true" %}обновления и настроек{% elsif firmware_here == "true" %}обновления{% elsif config_here == "true" %}настроек{% else %}настроек{% endif %}{% if paged == "true" %}
/// (поверх — `flash_page::ErasePage` со своей страницей стирания){% endif %}.
type Whole = BlockingPartition<'static, NoopRawMutex, ChipFlash>;
{%- endif %}
{%- if firmware_here == "true" %}

/// Флеш под `DFU`: общий `Flash` чипа, видимый асинхронно (`BlockingAsync`) —
/// порт OTA асинхронный.
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
/// флеша (`flash_page::ErasePage`): `DFU` обязан видеть ту же страницу, что
/// `ACTIVE` и `DFU` в bootloader'е, иначе размеры и смещения разделов у них
/// разойдутся.
type DfuFlash = BlockingAsync<ErasePage<Whole, SWAP_PAGE_SIZE>>;
{%- else %}
type DfuFlash = BlockingAsync<Whole>;
{%- endif %}

{%- if state_paged == "true" %}

/// Флеш под `BOOTLOADER_STATE`: тот же общий `Flash`, но со страницей
/// [`STATE_ERASE_SIZE`].
type StateFlash = BlockingAsync<ErasePage<Whole, STATE_ERASE_SIZE>>;
{%- else %}

/// Флеш под `BOOTLOADER_STATE`: тот же общий `Flash` — страница сектора `STATE`
/// совпадает с его страницей.
type StateFlash = BlockingAsync<Whole>;
{%- endif %}
{%- endif %}
{%- if config_here == "true" %}

/// Общий `Flash` чипа, видимый асинхронно{% if config_paged == "true" %}, со страницей сектора `CONFIG`
/// ([`CONFIG_ERASE_SIZE`], `ErasePage`){% endif %} — то, над чем строится раздел.
{%- if config_paged == "true" %}
type ConfigBacking = BlockingAsync<ErasePage<Whole, CONFIG_ERASE_SIZE>>;
{%- else %}
type ConfigBacking = BlockingAsync<Whole>;
{%- endif %}
{%- endif %}
{%- if eeprom_here == "true" %}

/// Общий [`FlashMutex`] как источник EEPROM. Свой тип, а не `impl` на самом
/// мьютексе: и трейт `Storage`, и `Mutex` чужие для `bsp`.
pub struct EepromFlash(&'static FlashMutex);

// Вручную: у `Flash` из `embassy-stm32` нет `Debug`.
impl core::fmt::Debug for EepromFlash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("EepromFlash").finish_non_exhaustive()
    }
}

impl ReadStorage for EepromFlash {
    type Error = embassy_stm32::flash::Error;

    fn read(&mut self, offset: u32, buf: &mut [u8]) -> Result<(), Self::Error> {
        self.0
            .lock(|flash| flash.borrow_mut().eeprom_read_slice(offset, buf))
    }

    fn capacity(&self) -> usize {
        embassy_stm32::flash::EEPROM_SIZE
    }
}

impl Storage for EepromFlash {
    fn write(&mut self, offset: u32, data: &[u8]) -> Result<(), Self::Error> {
        self.0
            .lock(|flash| flash.borrow_mut().eeprom_write_slice(offset, data))
    }
}
{%- endif %}

{%- if flash_cache == "true" %}
/// Флеш чипа, после стирания и записи которого кэш не отдаёт старое.
///
/// Ядро читает флеш через кэш ({% if cache contains "scb_d" %}кэш данных ядра Cortex-M7{% elsif cache contains "icache" %}блок `ICACHE`{% else %}кэш данных акселератора флеша, `FLASH_ACR.DCEN`{% endif %}),
/// а стирание и запись идут мимо него, и `embassy-stm32` после них кэш не
/// сбрасывает: прочитанное до стирания так и читалось бы после, и
/// `sequential-storage` с `embassy-boot` видели бы стёртое старым (поймано на
/// NUCLEO-H743 target-тестом `prepare_erases_only_what_the_image_needs`). Кэши
/// включает `crate::cache::enable` — docs/clocks.md, «Кэши».
pub struct Coherent<F>(F);

// Вручную: у `Flash` из `embassy-stm32` нет `Debug`.
impl<F> core::fmt::Debug for Coherent<F> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Coherent").finish_non_exhaustive()
    }
}

impl<F> Coherent<F> {
    /// Сбрасывает кэш, видящий флеш: следующее чтение пойдёт во флеш.
{%- if cache contains "scb_d" %}
    #[expect(unsafe_code, reason = "обоснование — в комментарии ниже")]
{%- endif %}
    fn forget() {
{%- if cache contains "scb_d" %}
        // SAFETY: только обслуживание кэша, под замком флеша, на одном ядре;
        // `SCB` и `CPUID` берутся через `steal()`, как в `crate::cache`.
        let mut core = unsafe { cortex_m::Peripherals::steal() };
        // Весь кэш с выпиской грязных строк, а не по адресам: строки RAM
        // выписываются, а не теряются, и адреса разделов знать не нужно. На
        // кэше в 16 KiB это микросекунды против миллисекунд стирания.
        core.SCB.clean_invalidate_dcache(&mut core.CPUID);
{%- endif %}
{%- if cache contains "icache" %}
        embassy_stm32::pac::ICACHE
            .cr()
            .modify(|w| w.set_cacheinv(true));
        while embassy_stm32::pac::ICACHE.sr().read().busyf() {}
{%- endif %}
{%- if cache contains "dcen" %}
        let acr = embassy_stm32::pac::FLASH.acr();
        // Сброс — только при выключенном кэше данных (RM, `FLASH_ACR.DCRST`).
        if acr.read().dcen() {
            acr.modify(|w| w.set_dcen(false));
            acr.modify(|w| w.set_dcrst(true));
            acr.modify(|w| w.set_dcrst(false));
            acr.modify(|w| w.set_dcen(true));
        }
{%- endif %}
    }
}

impl<F: ErrorType> ErrorType for Coherent<F> {
    type Error = F::Error;
}

impl<F: ReadNorFlash> ReadNorFlash for Coherent<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.0.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.0.capacity()
    }
}

impl<F: NorFlash> NorFlash for Coherent<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        let result = self.0.erase(from, to);
        // И при отказе: часть страниц могла успеть стереться.
        Self::forget();
        result
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        let result = self.0.write(offset, bytes);
        Self::forget();
        result
    }
}

{%- endif %}
/// `&'static` он не для красоты: носители {% if firmware_here == "true" and config_here == "true" %}обновления и настроек{% elsif firmware_here == "true" %}обновления{% elsif config_here == "true" %}настроек{% else %}настроек{% endif %} отдаются в
/// задачи, а аргументы задач embassy обязаны быть `'static`. Обычное поле
/// `Board` этого не даёт — `Board` живёт в `main`, а не вечно.
static FLASH: StaticCell<FlashMutex> = StaticCell::new();
{%- if dfu_bank == "true" %}

/// `&'static` — по той же причине, что [`FLASH`].
static DFU_BANK: StaticCell<DfuBankMutex> = StaticCell::new();
{%- endif %}
{%- if firmware_here == "true" %}

/// Свои асинхронные мьютексы носителям `DFU` и `BOOTLOADER_STATE`:
/// `embassy-boot` строит каждый раздел из `&Mutex<_, T>` своего типа. Внутри
/// обоих — ссылка на общий [`FlashMutex`], а не второй `Flash`: контроллер на
/// чипе один.
static DFU_FLASH: StaticCell<mutex::Mutex<NoopRawMutex, DfuFlash>> = StaticCell::new();
static STATE_FLASH: StaticCell<mutex::Mutex<NoopRawMutex, StateFlash>> = StaticCell::new();
{%- endif %}
{%- if config_here == "true" %}

/// Свой асинхронный мьютекс разделу `CONFIG`: `Partition` строится из
/// `&Mutex<_, T>`. Внутри — ссылка на общий [`FlashMutex`], а не второй
/// `Flash`: контроллер на чипе один. `NoopRawMutex` — то же допущение «один
/// исполнитель», что у `FlashMutex`{% if firmware_here == "true" %} и носителей обновления{% endif %}; менять вместе.
static CONFIG_FLASH: StaticCell<mutex::Mutex<NoopRawMutex, ConfigBacking>> = StaticCell::new();
{%- endif %}

/// Забирает контроллер флеша и заводит общий замок. Зовётся один раз.
{%- if dfu_bank == "true" %}
///
/// Вторым идёт банк 2 — под `DFU`.
pub(crate) fn new(
    resources: crate::resources::FlashResources,
) -> (&'static FlashMutex, &'static DfuBankMutex) {
    let banks = embassy_stm32::flash::Flash::new_blocking(resources.flash).into_blocking_regions();
    (
{%- if flash_cache == "true" %}
        FLASH.init(blocking_mutex::Mutex::new(RefCell::new(Coherent(
            banks.bank1_region,
        )))),
        DFU_BANK.init(blocking_mutex::Mutex::new(RefCell::new(Coherent(
            banks.bank2_region,
        )))),
{%- else %}
        FLASH.init(blocking_mutex::Mutex::new(RefCell::new(banks.bank1_region))),
        DFU_BANK.init(blocking_mutex::Mutex::new(RefCell::new(banks.bank2_region))),
{%- endif %}
    )
}
{%- else %}
pub(crate) fn new(resources: crate::resources::FlashResources) -> &'static FlashMutex {
    FLASH.init(blocking_mutex::Mutex::new(RefCell::new({% if flash_cache == "true" %}Coherent({% endif %}
        embassy_stm32::flash::Flash::new_blocking(resources.flash),
    {% if flash_cache == "true" %}){% endif %})))
}
{%- endif %}
{%- if firmware_here == "true" %}

/// Нарезает разделы обновления: `DFU` и `BOOTLOADER_STATE` над общим
/// [`FlashMutex`] со своими страницами — по символам `memory.x`, как их
/// режет bootloader. Границы обязаны быть кратны страницам, иначе
/// `Partition::new` паникует на старте: в сгенерированном `memory.x` это
/// держит `ASSERT` (падает линковка), в заполненном руками — никто.
{%- if dfu_bank == "true" %}
///
/// `DFU` — в банке 2, `STATE` — в банке 1.
pub(crate) fn firmware(
    flash: &'static FlashMutex,
    dfu_bank: &'static DfuBankMutex,
) -> FirmwareFlash {
    let dfu = BlockingAsync::new(BlockingPartition::new(dfu_bank, 0, DFU_BANK_SIZE));
{%- elsif swap_paged == "true" %}
pub(crate) fn firmware(flash: &'static FlashMutex) -> FirmwareFlash {
    let dfu = BlockingAsync::new(ErasePage::new(BlockingPartition::new(flash, 0, SIZE)));
{%- else %}
pub(crate) fn firmware(flash: &'static FlashMutex) -> FirmwareFlash {
    let dfu = BlockingAsync::new(BlockingPartition::new(flash, 0, SIZE));
{%- endif %}
{%- if state_paged == "true" %}
    let state = BlockingAsync::new(ErasePage::new(BlockingPartition::new(flash, 0, SIZE)));
{%- else %}
    let state = BlockingAsync::new(BlockingPartition::new(flash, 0, SIZE));
{%- endif %}
    let (dfu_start, dfu_len) = crate::layout::dfu();
    let (state_start, state_len) = crate::layout::state();
    adapters::ota::Partitions {
        dfu: Partition::new(DFU_FLASH.init(mutex::Mutex::new(dfu)), dfu_start, dfu_len),
        state: Partition::new(
            STATE_FLASH.init(mutex::Mutex::new(state)),
            state_start,
            state_len,
        ),
        capacity: crate::layout::active_len(),
        layout: crate::layout::fingerprint(),
    }
}
{%- endif %}
{%- if config_here == "true" %}

/// Собирает раздел `CONFIG`. Его границы кратны странице [`ConfigBacking`]
/// (`ASSERT` в `memory.x`{% if config_paged != "true" %}; `CONFIG_ERASE_SIZE` равен странице цельного `Flash` —
/// `const`-assert выше{% endif %}): иначе `Partition::new` паникует.
pub(crate) fn config(flash: &'static FlashMutex) -> ConfigFlash {
{%- if config_paged == "true" %}
    let backing = BlockingAsync::new(ErasePage::new(BlockingPartition::new(flash, 0, SIZE)));
{%- else %}
    let backing = BlockingAsync::new(BlockingPartition::new(flash, 0, SIZE));
{%- endif %}
    let backing = CONFIG_FLASH.init(mutex::Mutex::new(backing));
    let (start, len) = crate::layout::config();
    Partition::new(backing, start, len)
}
{%- endif %}
{%- if eeprom_here == "true" %}

/// Собирает внутреннюю EEPROM как `NorFlash`.
pub(crate) fn config(flash: &'static FlashMutex) -> ConfigFlash {
    adapters::eeprom::Eeprom::new(EepromFlash(flash))
}
{%- endif %}
