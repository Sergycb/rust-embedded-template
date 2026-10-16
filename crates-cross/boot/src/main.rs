#![no_std]
#![no_main]

use core::cell::RefCell;
{% if ota_store != "external" %}
use adapters::flash::{Paged, Shared};
{%- endif %}
use defmt::info;
// RTT — дефолтный транспорт в обоих профилях (см. crates-cross/app/src/main.rs);
// boot вдобавок не спавнит embassy-задач, поэтому очередь+drain-таск под
// USB/UART (см. graph.rs) сюда в принципе не встраивается —
// одну диагностическую строку перед прыжком не стоит того усложнять.
use defmt_rtt as _;
{%- if ota_store == "external" and signed == "true" %}
use embassy_boot_stm32::{BootLoader, BootLoaderConfig, State};
{%- elsif ota_store == "external" %}
use embassy_boot_stm32::BootLoaderConfig;
{%- else %}
use embassy_boot_stm32::{BootLoader, BootLoaderConfig};
{%- endif %}
use embassy_stm32::flash::{FLASH_BASE, Flash};
{%- if graph == "true" %}
#[cfg(not(debug_assertions))]
use embassy_stm32::wdg::IndependentWatchdog;
{%- endif %}
use embassy_sync::blocking_mutex::Mutex;
{% if bank_switch != "" %}
// Переключение флеша в одобанковый режим — тот же исходник, что у `bsp`:
// bootloader не зависит от `bsp`.
#[path = "../../bsp/src/banks.rs"]
mod banks;
{%- endif %}
{%- if ota_store == "external" %}
mod external;
{%- endif %}
{%- if graph == "true" %}
#[cfg(not(debug_assertions))]
mod fed;
{%- endif %}
// Паникёр по профилю — копия паникёра `app` (boot намеренно не зависит ни от
// `bsp`, ни от `app`). Дамп bootloader никогда не читает (это делает `app` при
// старте), так что причина падения bootloader'а в release лежит в PANIC до
// первого удачного старта приложения или до `cargo xtask panic-info`.
mod panic;
{%- if signed == "true" %}
mod signature;
{%- endif %}

#[cortex_m_rt::entry]
fn main() -> ! {
{%- if bank_switch != "" %}
    // Первым делом: `Flash::new` ниже паникует на двухбанковом флеше при
    // одобанковой раскладке (см. `banks`).
    banks::ensure_single_bank();
{%- endif %}
    let p = init_peripherals();
{%- if dfu_bank == "true" %}
    // Банк 2 стоит за разрывом, и цельный `Flash` туда не дотягивается (его
    // `FLASH_SIZE` — сумма банков, меньше смещения банка 2): флеш разбирается на
    // регионы банков. Банк 1 — bootloader, `STATE` и `ACTIVE`, банк 2 — `DFU`;
    // смещения в `memory.x` у `DFU` отсчитаны от начала банка 2.
    let banks = Flash::new_blocking(p.FLASH).into_blocking_regions();
    let (flash, dfu): (Bank1, Bank2) = (banks.bank1_region, banks.bank2_region);
{%- else %}
    let flash = Flash::new_blocking(p.FLASH);
{%- endif %}
{%- if ota_store == "external" %}
    // `STATE` и `DFU` — на внешней флеши. Драйвер блокирующий (в приложении —
    // асинхронный, `bsp::external`), собирается здесь из периферии `p` (SPI и
    // вывод CS). В шаблоне на его месте заглушка: каждая операция отвечает
    // ошибкой, и bootloader загружает текущий образ, не обменивая разделы.
    let external: external::ExternalFlash = adapters::flash::Unconnected;
{%- endif %}
{%- if signed == "true" %}
    // Новый образ проверяется ДО сторожа: проверка — один неделимый вызов
    // (сотня миллионов тактов на частоте после сброса), покормить сторож
    // внутри нечем. См. `signature::screen`.
{%- if ota_store == "external" %}
    let (flash, external, swap_allowed) = signature::screen(flash, external);
{%- elsif dfu_bank == "true" %}
    let (flash, dfu) = signature::screen(flash, dfu);
{%- else %}
    let flash = signature::screen(flash);
{%- endif %}
{%- endif %}

    // `Flash` (до `.into_blocking_regions()`) сам реализует `NorFlash` на
    // весь диапазон flash — erase/write внутри учитывают реальные границы
    // секторов чипа, даже неравномерные (F4/F7/H7: сектора внутри банка
    // разного размера). `.into_blocking_regions().bank1_region` — для чипов
    // с ОДНИМ равномерным регионом (F0/F1/F3/G0/L0/L1 и т.п.) даёт то же
    // самое, но для F4/F7/H7 региона с таким именем просто нет — там
    // `bank1_region1`/`bank1_region2`/`bank1_region3` (по одному на зону с
    // одинаковым размером сектора), и число регионов зависит от чипа.
    // Цельный `Flash` — единственный вариант, не завязанный на семейство.
{%- if dfu_bank == "true" %}
    // Кроме этого чипа: банк 2 стоит за разрывом, и регионы банков выше — не
    // выбор, а единственный способ до него дотянуться (см. начало `main`).
{%- endif %}
{%- if graph == "true" %}
    //
    // В release флеш{% if ota_store == "external" %} — оба, внутренний и внешний, —{% endif %} обёрнут сторожем (см. [`fed::Fed`]),
    // запущенным здесь же — до обмена разделов, но после проверки нового
    // образа (если она есть).
    #[cfg(not(debug_assertions))]
    let watchdog = {
        let mut watchdog = IndependentWatchdog::new(p.{{watchdog_peripheral}}, fed::WATCHDOG_TIMEOUT_US);
        watchdog.unleash();
        RefCell::new(watchdog)
    };
    #[cfg(not(debug_assertions))]
    let flash = fed::Fed::new(flash, &watchdog);
{%- if ota_store == "external" %}
    #[cfg(not(debug_assertions))]
    let external = fed::Fed::new(external, &watchdog);
{%- endif %}
{%- if dfu_bank == "true" %}
    #[cfg(not(debug_assertions))]
    let dfu = fed::Fed::new(dfu, &watchdog);
{%- endif %}
{%- endif %}
    let flash = Mutex::new(RefCell::new(flash));
{%- if ota_store == "external" %}
    // `STATE` и `DFU` — над одним драйвером: свой мьютекс на микросхему, а не
    // по объекту на раздел.
    let external = Mutex::new(RefCell::new(external));

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &external, &external);
    let active_offset = config.active.offset();
{%- if signed == "true" %}
    let bl = if swap_allowed {
        external::prepare(config)
    } else {
        BootLoader { state: State::Boot }
    };
{%- else %}
    let bl = external::prepare(config);
{%- endif %}
{%- else %}
    // `BOOTLOADER_STATE` — со страницей своего сектора, а не всего чипа (см.
    // `STATE_ERASE_SIZE`), но над тем же `Flash`: свой мьютекс, а не второй
    // объект контроллера.
    let state = Mutex::new(RefCell::new(Paged::<_, STATE_ERASE_SIZE>::new(
        Shared::new(&flash),
    )));
{%- if dfu_bank == "true" %}
    let dfu = Mutex::new(RefCell::new(dfu));

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &dfu, &state);
{%- else %}

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &state);
{%- endif %}
    let active_offset = config.active.offset();
    let bl = BootLoader::prepare::<_, _, _, {{write_size}}>(config);
{%- endif %}

    // Минимальный bootloader: как и официальный пример embassy-boot-stm32,
    // прыгает в активный образ безусловно, не проверяя валидность вектора
    // сброса/SP — повреждённый образ (прерванная прошивка, битый DFU) даст
    // HardFault вместо отказа с диагностикой. Полная проверка целостности
    // требует chip-specific границ RAM и не входит в минимальный шаблон.
    // `usize` на Cortex-M — 32 бита, усечения нет.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "`usize` на Cortex-M — 32 бита"
    )]
    let entry = FLASH_BASE as u32 + active_offset;
    info!("boot: jumping to app at {:x}", entry);
{%- if graph == "true" %}
    // Полный таймаут — приложению: до первого кормления из графа ему нужно
    // поднять HAL и запустить граф.
    #[cfg(not(debug_assertions))]
    watchdog.borrow_mut().pet();
{%- endif %}
    // SAFETY: `load` ставит VTOR на этот адрес, читает из образа по нему
    // начальный MSP (первое слово) и reset-вектор (второе) и прыгает — за этой
    // строкой не живёт ни один инвариант Rust, в том числе паникёр и таблица
    // векторов, на которых держится release-сборка.
    //
    // Предпосылки — раскладка и записанный образ, и обе сводятся к файлам:
    // * `entry` — это `FLASH_BASE` плюс смещение раздела ACTIVE, которое
    //   `BootLoaderConfig::from_linkerfile_blocking` берёт из символов
    //   `__bootloader_active_*`, то есть из `ORIGIN(FLASH)` и `LENGTH(FLASH)`
    //   в `crates-cross/boot/memory.x`; раскладку приложения с линковкой
    //   сверяют `cargo xtask build` и прошивка перед тестами на плате.
    // * первое и второе слова по этому адресу — MSP и reset-вектор образа,
    //   которые линкер положил в тот же раздел. Что bootloader их не
    //   проверяет — оговорка выше, про повреждённый образ.
    //
    // `#[expect(unsafe_code)]` — как и остальные unsafe в `crates-cross` (см. `crates-cross/Cargo.toml`)
    // (`app/src/main.rs`, блок линкерных символов в `bsp/src/ota.rs`): линт требует, чтобы каждое такое место
    // было перечислено явно.
    #[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
    unsafe {
        bl.load(entry)
    }
}
{%- if ota_store != "external" %}

/// Страница раздела `BOOTLOADER_STATE` — размер сектора, в котором он лежит.
///
/// Цельный `Flash` объявляет страницей самый крупный сектор чипа, а
/// `Partition::new` паникует на разделе, не кратном странице. На F2/F4/F7
/// `STATE` лежит в мелком секторе (у F4 — 16 KiB, а с разделом настроек
/// 64 KiB при странице 128 KiB), поэтому его страница своя. Число посчитано при
/// генерации тем же расчётом, что адреса в `memory.x`; то же самое стоит в
/// `bsp::ota` — разделу в приложении и в bootloader'е расходиться нельзя.
const STATE_ERASE_SIZE: usize = {{state_erase_size}};
{%- if dfu_bank == "true" %}

/// Банк 1 — bootloader, `STATE`, `ACTIVE` (смещения от базы flash, как у
/// цельного `Flash`).
type Bank1 = embassy_stm32::flash::Bank1Region<'static, embassy_stm32::flash::Blocking>;

/// Банк 2 — `DFU`, со смещениями от своего начала (символы `memory.x`
/// посчитаны так же).
type Bank2 = embassy_stm32::flash::Bank2Region<'static, embassy_stm32::flash::Blocking>;
{%- endif %}
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
