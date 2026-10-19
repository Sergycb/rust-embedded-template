#![no_std]
#![no_main]

use core::cell::RefCell;
{% if boot_partitioned == "true" %}
use adapters::flash::ErasePage;
{%- endif %}
{%- if state_paged == "true" %}
use bsp::ota::STATE_ERASE_SIZE;
{%- endif %}
{%- if swap_paged == "true" and dfu_bank != "true" %}
use bsp::ota::SWAP_PAGE_SIZE;
{%- endif %}
use defmt::info;
// RTT — дефолтный транспорт в обоих профилях (см. crates-cross/app/src/main.rs);
// boot вдобавок не спавнит embassy-задач, поэтому очередь+drain-таск под
// USB/UART (паттерн — docs/modules/app-graph.md) сюда в принципе не
// встраивается — одну диагностическую строку перед прыжком не стоит того
// усложнять.
use defmt_rtt as _;
{%- if ota_store == "external" and signed == "true" %}
use embassy_boot_stm32::{BootLoader, BootLoaderConfig, State};
{%- elsif ota_store == "external" %}
use embassy_boot_stm32::BootLoaderConfig;
{%- else %}
use embassy_boot_stm32::{BootLoader, BootLoaderConfig};
{%- endif %}
{%- if boot_partitioned == "true" %}
use embassy_embedded_hal::flash::partition::BlockingPartition;
{%- endif %}
use embassy_stm32::flash::{FLASH_BASE, Flash};
{%- if graph == "true" %}
#[cfg(not(feature = "no-watchdog"))]
use embassy_stm32::wdg::IndependentWatchdog;
{%- endif %}
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
{%- if ota_store == "external" %}
mod external;
{%- endif %}
{%- if signed == "true" %}
mod signature;
{%- endif %}
// Паникёр — `bsp::panic` (фича `panic-handler`), release-ветка: bootloader
// собирается только в release. Дамп bootloader никогда не читает (это делает
// `app` при старте), так что причина падения bootloader'а лежит в PANIC до
// первого удачного старта приложения или до `cargo xtask panic-info`.

#[bsp::cpu::entry]
fn main() -> ! {
{%- if bank_switch != "" %}
    // Первым делом: `Flash::new` ниже паникует на двухбанковом флеше при
    // одобанковой раскладке (см. `bsp::banks`).
    bsp::banks::ensure_single_bank();
{%- endif %}
    // На умолчаниях HAL, а не на конфиге платы: тактирование настраивает
    // приложение, bootloader'у хватает HSI.
    let p = bsp::board::init_peripherals(embassy_stm32::Config::default());
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
    // Новый образ проверяется до обмена разделов: см. `signature::screen`.
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
    let flash = Mutex::<NoopRawMutex, _>::new(RefCell::new(flash));
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
{%- if state_paged == "true" %}
    // `BOOTLOADER_STATE` — со страницей своего сектора, а не всего чипа (см.
    // `STATE_ERASE_SIZE`), но над тем же `Flash`: свой мьютекс, а не второй
    // объект контроллера.
    let state = Mutex::new(RefCell::new(ErasePage::<_, STATE_ERASE_SIZE>::new(
        BlockingPartition::new(&flash, 0, FLASH_BYTES),
    )));
{%- endif %}
{%- if dfu_bank == "true" %}
    let dfu = Mutex::new(RefCell::new(dfu));

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &dfu, {% if state_paged == "true" %}&state{% else %}&flash{% endif %});
{%- elsif swap_paged == "true" %}
    // `ACTIVE` и `DFU` — со страницей обмена `SWAP_PAGE_SIZE`, крупнее физической
    // страницы флеша (см. её описание): один мьютекс на оба раздела, как и
    // раньше у цельного `Flash`.
    let swap = Mutex::new(RefCell::new(ErasePage::<_, SWAP_PAGE_SIZE>::new(
        BlockingPartition::new(&flash, 0, FLASH_BYTES),
    )));

    let config = BootLoaderConfig::from_linkerfile_blocking(&swap, &swap, {% if state_paged == "true" %}&state{% else %}&flash{% endif %});
{%- else %}

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, {% if state_paged == "true" %}&state{% else %}&flash{% endif %});
{%- endif %}
    let active_offset = config.active.offset();
    let bl = BootLoader::prepare::<_, _, _, SWAP_BUFFER_SIZE>(config);
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
    // Сторож — последним, прямо перед прыжком, и целиком достаётся приложению:
    // до первого кормления из графа ему нужно поднять HAL и запустить граф.
    // Раньше он не нужен: бутлоадер защищает от неисправного образа (не
    // стартует, не кормит), а не от собственного обмена разделов, сколько бы
    // тот ни длился. Остановить его уже нельзя — приложение лишь подхватывает
    // идущий счётчик (`bsp::wdg`, `arm()`). Без фичи `no-watchdog`: ею `cargo
    // xtask test target` собирает bootloader под тесты на плате, где кормить
    // сторож некому.
    #[cfg(not(feature = "no-watchdog"))]
    IndependentWatchdog::new(p.{{watchdog_peripheral}}, bsp::wdg::HW_TIMEOUT_US).unleash();
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
    // (`app/src/main.rs`, `app/src/image.rs`): линт требует, чтобы каждое такое место
    // было перечислено явно.
    #[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
    unsafe {
        bl.load(entry)
    }
}
{%- if ota_store != "external" %}

/// Буфер обмена `BootLoader::prepare` — слово записи флеша из генерации
/// (`write_size`, его можно переопределить `--define`). `prepare` требует,
/// чтобы буфер был кратен слову записи реального флеша; несовпадение — ошибка
/// сборки, а не паника на старте bootloader'а.
const SWAP_BUFFER_SIZE: usize = {{write_size}};

{%- if boot_partitioned == "true" and dfu_bank == "true" %}

/// Размер банка 1 в байтах — столько занимает раздел на весь флеш, из
/// которого строятся `STATE` и `ACTIVE`.
const FLASH_BYTES: u32 = embassy_stm32::flash::BANK1_REGION.size;
{%- elsif boot_partitioned == "true" %}

/// Размер всего флеша в байтах — столько занимает раздел на весь флеш, из
/// которого строятся `STATE`, `ACTIVE` и `DFU`.
///
/// `usize` на Cortex-M — 32 бита, усечения нет.
#[expect(
    clippy::cast_possible_truncation,
    reason = "`usize` на Cortex-M — 32 бита"
)]
const FLASH_BYTES: u32 = embassy_stm32::flash::FLASH_SIZE as u32;

// `BlockingPartition::new` паникует на объёме, не кратном `ERASE_SIZE`; у STM32
// объём всегда кратен самому крупному сектору, и сборка это проверяет, а не
// первый запуск на плате.
const _: () = assert!(
    embassy_stm32::flash::FLASH_SIZE.is_multiple_of(embassy_stm32::flash::MAX_ERASE_SIZE),
    "объём flash не кратен самому крупному сектору: `BlockingPartition::new` паникует на старте"
);
{%- endif %}

const _: () = assert!(
    SWAP_BUFFER_SIZE.is_multiple_of(embassy_stm32::flash::WRITE_SIZE),
    "write_size из генерации не кратен слову записи флеша чипа (embassy_stm32::flash::WRITE_SIZE): \
     пересоберите проект без --define write_size или поправьте значение"
);
{%- if dfu_bank == "true" %}

/// Банк 1 — bootloader, `STATE`, `ACTIVE` (смещения от базы flash, как у
/// цельного `Flash`).
type Bank1 = embassy_stm32::flash::Bank1Region<'static, embassy_stm32::flash::Blocking>;

/// Банк 2 — `DFU`, со смещениями от своего начала (символы `memory.x`
/// посчитаны так же).
type Bank2 = embassy_stm32::flash::Bank2Region<'static, embassy_stm32::flash::Blocking>;
{%- endif %}
{%- endif %}
