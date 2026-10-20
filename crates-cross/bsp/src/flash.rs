#![doc = include_str!("../../../docs/modules/bsp-flash.md")]

use core::cell::RefCell;

use embassy_embedded_hal::flash::partition::BlockingPartition;
{%- if dfu_bank == "true" %}
use embassy_stm32::flash::{BANK1_REGION, BANK2_REGION, Bank1Region, Bank2Region, Blocking};
{%- else %}
use embassy_stm32::flash::{Blocking, FLASH_SIZE, MAX_ERASE_SIZE};
{%- endif %}
use embassy_stm32::{Peri, peripherals};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use static_cell::StaticCell;
{%- if dfu_bank != "true" %}

/// Размер всего флеша в байтах — столько занимает раздел [`Whole`].
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

/// Размер банка 1 в байтах — столько занимает раздел [`Whole`]. Банк, а не
/// весь чип: банк 2 стоит за разрывом, и цельный `Flash` туда не дотягивается
/// (`FLASH_SIZE` у него — сумма банков, меньше смещения банка 2).
pub const SIZE: u32 = BANK1_REGION.size;

/// Размер банка 2 в байтах — столько занимает раздел под `DFU`.
pub const DFU_BANK_SIZE: u32 = BANK2_REGION.size;
{%- endif %}

/// `&'static` он не для красоты: разделы OTA и `Settings` отдаются в задачи, а
/// аргументы задач embassy обязаны быть `'static`. Обычное поле `Board` этого
/// не даёт — `Board` живёт в `main`, а не вечно.
static FLASH: StaticCell<FlashMutex> = StaticCell::new();
{%- if dfu_bank == "true" %}

/// `&'static` — по той же причине, что [`FLASH`].
static DFU_BANK: StaticCell<DfuBankMutex> = StaticCell::new();
{%- endif %}

/// Общий `Flash` всего чипа под блокирующим замком. Контроллер флеша один, а
/// работать с ним нужно и OTA (разделы `DFU`/`BOOTLOADER_STATE`), и настройкам
/// (раздел `CONFIG` во флеше или внутренняя EEPROM — она пишется через тот же
/// контроллер), — поэтому объект создаётся ровно один и раздаётся ссылками.
///
/// `NoopRawMutex`, а не `CriticalSectionRawMutex`: обе половины работают из
/// задач одного исполнителя, то есть без вытеснения. Появится второй
/// исполнитель (прерывательный приоритет, второе ядро) — менять здесь.
pub type FlashMutex = Mutex<NoopRawMutex, RefCell<ChipFlash>>;
{%- if dfu_bank == "true" %}

/// Флеш под [`FlashMutex`] — банк 1, а не весь чип. Смещения в банке 1 те же,
/// что у цельного `Flash`, — от базы flash.
pub type ChipFlash = Bank1Region<'static, Blocking>;

/// Банк 2 — под `DFU`, и только под него: смещения в нём отсчитаны от его
/// начала, и символы `memory.x` у `DFU` посчитаны так же.
pub type DfuBankMutex = Mutex<NoopRawMutex, RefCell<Bank2Region<'static, Blocking>>>;

/// Весь банк 2 как раздел под замком — под него строится `DFU`. Строится на
/// месте: `BlockingPartition::new(dfu_bank, 0, flash::DFU_BANK_SIZE)`.
pub type DfuBank = BlockingPartition<'static, NoopRawMutex, Bank2Region<'static, Blocking>>;
{%- else %}

/// Флеш под [`FlashMutex`] — весь чип одним объектом.
pub type ChipFlash = embassy_stm32::flash::Flash<'static, Blocking>;
{%- endif %}

/// Весь флеш как один раздел: смещения те же, что у самого флеша, а операции
/// идут под замком, по одной. Из него строятся разделы OTA и настроек{% if erase_page == "true" %}
/// (поверх — `flash_page::ErasePage` со своей страницей стирания){% endif %}.
///
/// Строится на месте: `BlockingPartition::new(flash, 0, flash::SIZE)`.
pub type Whole = BlockingPartition<'static, NoopRawMutex, ChipFlash>;

/// Забирает контроллер флеша и заводит общий замок. Зовётся один раз.
{%- if dfu_bank == "true" %}
///
/// Вторым идёт банк 2 — под `DFU`.
pub(crate) fn new(
    flash: Peri<'static, peripherals::FLASH>,
) -> (&'static FlashMutex, &'static DfuBankMutex) {
    let banks = embassy_stm32::flash::Flash::new_blocking(flash).into_blocking_regions();
    (
        FLASH.init(Mutex::new(RefCell::new(banks.bank1_region))),
        DFU_BANK.init(Mutex::new(RefCell::new(banks.bank2_region))),
    )
}
{%- else %}
pub(crate) fn new(flash: Peri<'static, peripherals::FLASH>) -> &'static FlashMutex {
    FLASH.init(Mutex::new(RefCell::new(
        embassy_stm32::flash::Flash::new_blocking(flash),
    )))
}
{%- endif %}
