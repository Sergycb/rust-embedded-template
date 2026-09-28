#![no_std]
#![no_main]

use core::cell::RefCell;

use defmt::info;
// RTT — дефолтный транспорт в обоих профилях (см. crates-cross/app/src/main.rs);
// boot вдобавок не спавнит embassy-задач, поэтому очередь+drain-таск под
// USB/UART (см. graph.rs) сюда в принципе не встраивается —
// одну диагностическую строку перед прыжком не стоит того усложнять.
use defmt_rtt as _;
use embassy_boot_stm32::{BootLoader, BootLoaderConfig};
use embassy_stm32::flash::{FLASH_BASE, Flash};
{%- if graph == "true" %}
#[cfg(not(debug_assertions))]
use embassy_stm32::wdg::IndependentWatchdog;
{%- endif %}
use embassy_sync::blocking_mutex::Mutex;
{%- if graph == "true" %}
#[cfg(not(debug_assertions))]
use embedded_storage::nor_flash::{ErrorType, NorFlash, ReadNorFlash};
{%- endif %}

// Паникёр по профилю — тот же, что у `app` (см. panic.rs). Дамп bootloader
// никогда не читает (это делает `app` при старте), так что причина падения
// bootloader'а в release лежит в PANIC до первого удачного старта приложения
// или до `cargo xtask panic`.
mod panic;

#[cortex_m_rt::entry]
fn main() -> ! {
    let p = init_peripherals();

    // `Flash` (до `.into_blocking_regions()`) сам реализует `NorFlash` на
    // весь диапазон flash — erase/write внутри учитывают реальные границы
    // секторов чипа, даже неравномерные (F4/F7/H7: сектора внутри банка
    // разного размера). `.into_blocking_regions().bank1_region` — для чипов
    // с ОДНИМ равномерным регионом (F0/F1/F3/G0/L0/L1 и т.п.) даёт то же
    // самое, но для F4/F7/H7 региона с таким именем просто нет — там
    // `bank1_region1`/`bank1_region2`/`bank1_region3` (по одному на зону с
    // одинаковым размером сектора), и число регионов зависит от чипа.
    // Цельный `Flash` — единственный вариант, не завязанный на семейство.
{%- if graph == "true" %}
    //
    // В release флеш обёрнут сторожем (см. [`Fed`]), запущенным здесь же —
    // первым делом, до обмена разделов.
    #[cfg(not(debug_assertions))]
    let flash = Mutex::new(RefCell::new(Fed::new(
        Flash::new_blocking(p.FLASH),
        IndependentWatchdog::new(p.{{watchdog_peripheral}}, WATCHDOG_TIMEOUT_US),
    )));
    #[cfg(debug_assertions)]
{%- endif %}
    let flash = Mutex::new(RefCell::new(Flash::new_blocking(p.FLASH)));

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &flash);
    let active_offset = config.active.offset();
    let bl = BootLoader::prepare::<_, _, _, {{write_size}}>(config);

    // Минимальный bootloader: как и официальный пример embassy-boot-stm32,
    // прыгает в активный образ безусловно, не проверяя валидность вектора
    // сброса/SP — повреждённый образ (прерванная прошивка, битый DFU) даст
    // HardFault вместо отказа с диагностикой. Полная проверка целостности
    // требует chip-specific границ RAM и не входит в минимальный шаблон.
    let entry = FLASH_BASE as u32 + active_offset;
    info!("boot: jumping to app at {:x}", entry);
{%- if graph == "true" %}
    // Полный таймаут — приложению: до первого кормления из графа ему нужно
    // поднять HAL и подтвердить образ.
    #[cfg(not(debug_assertions))]
    flash.lock(|flash| flash.borrow_mut().watchdog.pet());
{%- endif %}
    unsafe { bl.load(entry) }
}
{%- if graph == "true" %}

/// Таймаут сторожа bootloader'а — он же у приложения до первого кормления из
/// графа.
///
/// Должен пережить самую долгую ОДНУ операцию флеша ([`Fed`] кормит между
/// ними) — стирание страницы обмена, то есть крупнейшего сектора: секунды на
/// F4/H7, — и путь приложения
/// от прыжка до тикера графа (подъём HAL, `mark_booted`). Верхний предел —
/// диапазон IWDG, 26.2 с при LSI 40 кГц.
#[cfg(not(debug_assertions))]
const WATCHDOG_TIMEOUT_US: u32 = 10_000_000;

/// Флеш, кормящий сторож перед каждым стиранием и записью.
///
/// Обмен разделов внутри `BootLoader::prepare` — один блокирующий вызов, и на
/// крупных секторах он длится дольше любого таймаута IWDG: кормить снаружи его
/// нечем. Изнутри — можно: всё, что делает обмен, идёт через этот объект.
///
/// Только release и только в проекте с графом. В debug-профиле bootloader'ом
/// пользуются target-тесты (`cargo xtask test target`), а кормить сторож в
/// них некому; без графа — то же самое в самом приложении.
///
/// Зачем сторож здесь вообще: IWDG, запущенный однажды, до сброса не
/// останавливается. Новый образ, зависший раньше тикера графа, сбрасывается
/// им, не подтвердив себя (`mark_booted`), и следующий старт bootloader'а
/// откатывает его — без сторожа плата висела бы до выключения питания.
#[cfg(not(debug_assertions))]
struct Fed<F> {
    flash: F,
    watchdog: IndependentWatchdog<'static, embassy_stm32::peripherals::{{watchdog_peripheral}}>,
}

#[cfg(not(debug_assertions))]
impl<F> Fed<F> {
    fn new(
        flash: F,
        mut watchdog: IndependentWatchdog<'static, embassy_stm32::peripherals::{{watchdog_peripheral}}>,
    ) -> Self {
        watchdog.unleash();
        Self { flash, watchdog }
    }
}

#[cfg(not(debug_assertions))]
impl<F: ErrorType> ErrorType for Fed<F> {
    type Error = F::Error;
}

#[cfg(not(debug_assertions))]
impl<F: ReadNorFlash> ReadNorFlash for Fed<F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.flash.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.flash.capacity()
    }
}

#[cfg(not(debug_assertions))]
impl<F: NorFlash> NorFlash for Fed<F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    /// Диапазон не дробится: `embassy-boot` сам стирает по одной странице
    /// (крупнейший сектор чипа) за вызов, а дробить по `ERASE_SIZE` здесь
    /// нельзя — на F4/F7/H7 секторы неравные, и граница посреди настоящего
    /// сектора дала бы отказ стирания.
    fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
        self.watchdog.pet();
        self.flash.erase(from, to)
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.watchdog.pet();
        self.flash.write(offset, bytes)
    }
}
{%- endif %}

{%- if dual_core == "true" %}

// См. тот же приём и обоснование в crates-cross/bsp/src/board.rs — здесь дублируется,
// а не выносится в общий крейт: boot намеренно не зависит от bsp.
fn init_peripherals() -> embassy_stm32::Peripherals {
    static SHARED_DATA: core::mem::MaybeUninit<embassy_stm32::SharedData> =
        core::mem::MaybeUninit::uninit();
    embassy_stm32::init_primary(embassy_stm32::Config::default(), &SHARED_DATA)
}
{%- else %}

fn init_peripherals() -> embassy_stm32::Peripherals {
    embassy_stm32::init(embassy_stm32::Config::default())
}
{%- endif %}
