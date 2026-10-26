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

use core::cell::RefCell;
{% if whole == "true" %}
use embassy_embedded_hal::adapter::BlockingAsync;
use embassy_embedded_hal::flash::partition::BlockingPartition;
{%- endif %}
{%- if config_here == "true" %}
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
{%- if paged == "true" %}
use flash_page::ErasePage;
{%- endif %}
{%- if eeprom_here == "true" %}
use ports::EepromAccess;
{%- endif %}
use static_cell::StaticCell;
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
{%- if firmware_here == "true" %}

/// Страница раздела `BOOTLOADER_STATE` — размер сектора, в котором он лежит.
///
/// Цельный `Flash` объявляет страницей самый крупный сектор чипа, а
/// `Partition::new` паникует на разделе, не кратном странице. На F2/F4/F7
/// `STATE` лежит в мелком секторе (у F4 — 16 KiB, а с разделом настроек
/// 64 KiB при странице 128 KiB), поэтому его страница своя{% if state_paged != "true" %}; у этого чипа
/// она равна странице цельного `Flash`, и раздел строится прямо над ним, а
/// равенство держит `const`-assert ниже{% endif %}. Число посчитано при
/// генерации тем же расчётом, что адреса в `memory.x`, и приезжает переменной
/// среды `STATE_ERASE_SIZE` из `[env]` `crates-cross/.cargo/config.toml`. Её
/// же берёт bootloader — отсюда, и разделу в приложении и в нём расходиться нельзя.
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
/// Генерация берёт её больше физической, чтобы журнал `BOOTLOADER_STATE`
/// (запись на каждую страницу `ACTIVE`) стал короче и коду bootloader'а
/// остался почти весь его блок. Флеш под `DFU` — `ErasePage<_, SWAP_PAGE_SIZE>`;
/// `ACTIVE` с той же страницей собирает bootloader (отсюда же берёт и число).
/// Число — `page_size` из генерации, переменная среды `SWAP_PAGE_SIZE`; кратно
/// оно странице стирания флеша (проверка ниже).
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
{%- if config_here == "true" %}

/// Страница раздела `CONFIG` — размер сектора, в котором он лежит.
///
/// `sequential-storage` делит раздел на страницы по `ERASE_SIZE` флеша, а
/// цельный `Flash` объявляет им самый крупный сектор чипа. Раздел в мелких
/// секторах (F2/F4/F7{% if ota == "true" %} с OTA{% endif %}) со страницей всего чипа не делится на две, поэтому
/// страница своя{% if config_paged == "true" %} (`ErasePage`){% endif %}; в хвосте flash она совпадает со страницей чипа{% if config_paged != "true" %} — как у
/// этого чипа: раздел строится прямо над [`Whole`], а равенство держит
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

// Раздел строится как `Partition` над страницей [`CONFIG_ERASE_SIZE`], и
// `Partition::new` паникует на старте, если его границы не кратны ей. Расчёт
// раскладки в генерации держит их на границах, а это — проверка при компиляции,
// чтобы ручная правка `[env]` не превратилась в панику на плате.
const _: () = assert!(
    (crate::layout::CONFIG_START as usize).is_multiple_of(CONFIG_ERASE_SIZE)
        && (crate::layout::CONFIG_END as usize).is_multiple_of(CONFIG_ERASE_SIZE),
    "раздел CONFIG не на границах своей страницы (CONFIG_ERASE_SIZE): Partition::new паникует"
);
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
/// задач одного исполнителя, то есть без вытеснения. Появится второй
/// исполнитель (прерывательный приоритет, второе ядро) — менять здесь.
pub type FlashMutex = blocking_mutex::Mutex<NoopRawMutex, RefCell<ChipFlash>>;
{%- if dfu_bank == "true" %}

/// Флеш под [`FlashMutex`] — банк 1, а не весь чип. Смещения в банке 1 те же,
/// что у цельного `Flash`, — от базы flash.
pub type ChipFlash = Bank1Region<'static, Blocking>;

/// Банк 2 — под `DFU`, и только под него: смещения в нём отсчитаны от его
/// начала, и символы `memory.x` у `DFU` посчитаны так же.
pub type DfuBankMutex =
    blocking_mutex::Mutex<NoopRawMutex, RefCell<Bank2Region<'static, Blocking>>>;

/// Весь банк 2 как раздел под замком — под него строится `DFU`.
type DfuBank = BlockingPartition<'static, NoopRawMutex, Bank2Region<'static, Blocking>>;
{%- else %}

/// Флеш под [`FlashMutex`] — весь чип одним объектом.
pub type ChipFlash = embassy_stm32::flash::Flash<'static, Blocking>;
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

/// Флеш под обновлением — рычаг, над которым узел OTA собирает адаптер
/// (`adapters::ota::Updater`, порт `Assemble`). Разделы `DFU` и
/// `BOOTLOADER_STATE` в нём находит сам узел, по символам `memory.x`
/// (`adapters::ota::Linked`); плата отдаёт только флеш с нужной страницей и
/// размер `ACTIVE`.
pub type FirmwareFlash = adapters::ota::Linked<DfuFlash, StateFlash>;
{%- endif %}
{%- if config_here == "true" %}

/// Общий `Flash` чипа, видимый асинхронно{% if config_paged == "true" %}, со страницей сектора `CONFIG`
/// ([`CONFIG_ERASE_SIZE`], `ErasePage`){% endif %} — то, над чем строится раздел.
{%- if config_paged == "true" %}
type ConfigBacking = BlockingAsync<ErasePage<Whole, CONFIG_ERASE_SIZE>>;
{%- else %}
type ConfigBacking = BlockingAsync<Whole>;
{%- endif %}

/// Раздел `CONFIG` — носитель настроек, над которым узел-потребитель собирает
/// хранилище (`adapters::settings`, порт `Assemble`). Именно раздел, а не весь
/// флеш: запись за его границы отказывает, а не стирает {% if ota == "true" %}`ACTIVE` или bootloader{% else %}прошивку{% endif %}.
pub type ConfigFlash = Partition<'static, NoopRawMutex, ConfigBacking>;
{%- endif %}
{%- if eeprom_here == "true" %}

/// Внутренняя EEPROM как `NorFlash` — носитель настроек, над которым
/// узел-потребитель собирает хранилище (`adapters::settings`, порт `Assemble`).
/// Флеш под настройки не тратится вовсе: EEPROM пишется через тот же
/// контроллер, что и флеш, поэтому доступ к ней — через общий [`FlashMutex`]
/// (`embassy_stm32::flash::Flash::eeprom_*`), по операции под замком. Обёртку
/// `adapters::eeprom::Eeprom` (инверсия байтов, страница) собирает плата, и
/// под хранилище отдана вся EEPROM, округлённая до её страницы.
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
{%- if eeprom_here == "true" %}

/// `&'static` — обёртка `Eeprom` держит доступ к EEPROM ссылкой, а хранилище
/// уезжает в задачу.
static EEPROM: StaticCell<EepromFlash> = StaticCell::new();
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
        FLASH.init(blocking_mutex::Mutex::new(RefCell::new(banks.bank1_region))),
        DFU_BANK.init(blocking_mutex::Mutex::new(RefCell::new(banks.bank2_region))),
    )
}
{%- else %}
pub(crate) fn new(resources: crate::resources::FlashResources) -> &'static FlashMutex {
    FLASH.init(blocking_mutex::Mutex::new(RefCell::new(
        embassy_stm32::flash::Flash::new_blocking(resources.flash),
    )))
}
{%- endif %}
{%- if firmware_here == "true" %}

/// Собирает флеш под обновлением: носители `DFU` и `BOOTLOADER_STATE` над общим
/// [`FlashMutex`] со своими страницами и размер `ACTIVE`.
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
    adapters::ota::Linked::new(
        DFU_FLASH.init(mutex::Mutex::new(dfu)),
        STATE_FLASH.init(mutex::Mutex::new(state)),
        crate::layout::ACTIVE_LEN,
        crate::layout::fingerprint(),
    )
}
{%- endif %}
{%- if config_here == "true" %}

/// Собирает раздел `CONFIG`. Его начало кратно странице [`ConfigBacking`]{% if config_paged != "true" %}
/// (`CONFIG_ERASE_SIZE` равен странице цельного `Flash` — `const`-assert выше){% endif %}:
/// иначе `Partition::new` паникует.
pub(crate) fn config(flash: &'static FlashMutex) -> ConfigFlash {
{%- if config_paged == "true" %}
    let backing = BlockingAsync::new(ErasePage::new(BlockingPartition::new(flash, 0, SIZE)));
{%- else %}
    let backing = BlockingAsync::new(BlockingPartition::new(flash, 0, SIZE));
{%- endif %}
    let backing = CONFIG_FLASH.init(mutex::Mutex::new(backing));
    Partition::new(
        backing,
        crate::layout::CONFIG_START,
        crate::layout::CONFIG_END - crate::layout::CONFIG_START,
    )
}
{%- endif %}
{%- if eeprom_here == "true" %}

/// Собирает внутреннюю EEPROM как `NorFlash`.
pub(crate) fn config(flash: &'static FlashMutex) -> ConfigFlash {
    adapters::eeprom::Eeprom::new(EEPROM.init(EepromFlash(flash)))
}
{%- endif %}
