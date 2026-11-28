#![doc = include_str!("../../../docs/modules/cpu-flash.md")]
{%- assign flash_cache = "false" %}
{%- if cache contains "scb_d" or cache contains "icache" or cache contains "dcen" %}
{%- assign flash_cache = "true" %}
{%- endif %}
{%- assign firmware_here = "false" %}
{%- if ota == "true" %}
{%- if ota_store != "external" %}
{%- assign firmware_here = "true" %}
{%- endif %}
{%- endif %}

{%- if flash_cache == "true" %}
use crate::cache::Coherent;
{%- endif %}
use core::cell::RefCell;
{%- if dfu_bank == "true" %}
use embassy_stm32::flash::{BANK1_REGION, BANK2_REGION, Bank1Region, Bank2Region, Blocking};
{%- else %}
use embassy_stm32::flash::{Blocking, FLASH_SIZE};
{%- endif %}
use embassy_sync::blocking_mutex;
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
use static_cell::StaticCell;

pub use embassy_stm32::flash::{FLASH_BASE, MAX_ERASE_SIZE, WRITE_SIZE};
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
{%- if dfu_bank != "true" %}

/// Размер всего флеша в байтах — столько занимает образ приложения.
///
/// `usize` на Cortex-M — 32 бита, усечения нет.
#[expect(
    clippy::cast_possible_truncation,
    reason = "`usize` на Cortex-M — 32 бита"
)]
pub const SIZE: u32 = FLASH_SIZE as u32;

// `BlockingPartition::new` паникует на объёме, не кратном `ERASE_SIZE`; у STM32
// объём всегда кратен самому крупному сектору, и сборка это проверяет, а не
// первый запуск на плате.
const _: () = assert!(
    FLASH_SIZE.is_multiple_of(MAX_ERASE_SIZE),
    "объём flash не кратен самому крупному сектору: `BlockingPartition::new` паникует на старте"
);
{%- else %}

/// Размер банка 1 в байтах — столько занимает образ приложения. Банк, а не
/// весь чип: банк 2 стоит за разрывом, и цельный `Flash` туда не дотягивается
/// (`FLASH_SIZE` у него — сумма банков, меньше смещения банка 2).
pub const SIZE: u32 = BANK1_REGION.size;

/// Размер банка 2 в байтах — столько занимает раздел под `DFU`.
pub const BANK2_SIZE: u32 = BANK2_REGION.size;

/// Флеш под [`Lock`] — банк 1, а не весь чип. Смещения в банке 1 те же,
/// что у цельного `Flash`, — от базы flash.
pub type Bank1Flash = {% if flash_cache == "true" %}Coherent<Bank1Region<'static, Blocking>>{% else %}Bank1Region<'static, Blocking>{% endif %};

/// Банк 2 — под `DFU`, и только под него: смещения в нём отсчитаны от его
/// начала, и символы `memory.x` у `DFU` посчитаны так же.
pub type Bank2Flash = {% if flash_cache == "true" %}Coherent<Bank2Region<'static, Blocking>>{% else %}Bank2Region<'static, Blocking>{% endif %};
{%- endif %}

/// Флеш под [`Lock`]: весь чип (или банк 1) одним объектом.
pub type ChipFlash = {% if dfu_bank == "true" %}Bank1Flash{% elsif flash_cache == "true" %}Coherent<embassy_stm32::flash::Flash<'static, Blocking>>{% else %}embassy_stm32::flash::Flash<'static, Blocking>{% endif %};

/// Сырой флеш чипа под блокирующим замком — то, над чем строятся разделы.
///
/// `ThreadModeRawMutex` — то же допущение «один исполнитель», что у прочих
/// замков прошивки: все пользователи работают из задач одного исполнителя;
/// что менять вместе с ним — docs/flash.md.
pub type Lock = blocking_mutex::Mutex<ThreadModeRawMutex, RefCell<ChipFlash>>;
{%- if dfu_bank == "true" %}

/// Банк 2 под блокирующим замком: из него строится раздел `DFU`.
pub type BankMutex = blocking_mutex::Mutex<ThreadModeRawMutex, RefCell<Bank2Flash>>;
{%- endif %}

/// Ручка на сырой флеш чипа — то, что `Cpu::split` отдаёт плате.
///
/// `Copy` намеренно: флеш в прошивке один, а нужен он и обновлению, и
/// настройкам; плата отдаёт ручку в оба, а не двигает объект между ними.
#[derive(Clone, Copy)]
pub struct Flash {
    /// Общий флеш чипа.
    pub lock: &'static Lock,
{%- if dfu_bank == "true" %}
    /// Банк 2 — под `DFU`.
    pub bank2: &'static BankMutex,
{%- endif %}
}

// Вручную: у `Flash` из `embassy-stm32` нет `Debug`, а печатать замок незачем.
impl core::fmt::Debug for Flash {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Flash").finish_non_exhaustive()
    }
}

/// `&'static` он не для красоты: флеш отдаётся в задачи, а аргументы задач
/// embassy обязаны быть `'static`. Обычное поле `Board` этого не даёт —
/// `Board` живёт в `main`, а не вечно.
static FLASH: StaticCell<Lock> = StaticCell::new();
{%- if dfu_bank == "true" %}

/// `&'static` — по той же причине, что [`FLASH`].
static DFU_BANK: StaticCell<BankMutex> = StaticCell::new();
{%- endif %}

/// Забирает контроллер флеша и заводит общий замок. Зовётся один раз.
{%- if dfu_bank == "true" %}
///
/// Второй замок — банк 2, под `DFU`.
pub(crate) fn new(flash: embassy_stm32::Peri<'static, embassy_stm32::peripherals::FLASH>) -> Flash {
    let banks = embassy_stm32::flash::Flash::new_blocking(flash).into_blocking_regions();
    Flash {
{%- if flash_cache == "true" %}
        lock: FLASH.init(blocking_mutex::Mutex::new(RefCell::new(Coherent::new(
            banks.bank1_region,
        )))),
        bank2: DFU_BANK.init(blocking_mutex::Mutex::new(RefCell::new(Coherent::new(
            banks.bank2_region,
        )))),
{%- else %}
        lock: FLASH.init(blocking_mutex::Mutex::new(RefCell::new(banks.bank1_region))),
        bank2: DFU_BANK.init(blocking_mutex::Mutex::new(RefCell::new(banks.bank2_region))),
{%- endif %}
    }
}
{%- else %}
pub(crate) fn new(flash: embassy_stm32::Peri<'static, embassy_stm32::peripherals::FLASH>) -> Flash {
    Flash {
{%- if flash_cache == "true" %}
        lock: FLASH.init(blocking_mutex::Mutex::new(RefCell::new(Coherent::new(
            embassy_stm32::flash::Flash::new_blocking(flash),
        )))),
{%- else %}
        lock: FLASH.init(blocking_mutex::Mutex::new(RefCell::new(
            embassy_stm32::flash::Flash::new_blocking(flash),
        ))),
{%- endif %}
    }
}
{%- endif %}
