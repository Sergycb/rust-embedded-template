#![no_std]
#![no_main]

use core::cell::RefCell;

use adapters::flash::{Paged, Shared};
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

// Паникёр по профилю — тот же исходник, что у `app`: модуль подключён по
// пути, а не копией и не общим крейтом (boot намеренно не зависит ни от
// `bsp`, ни от `app`). Дамп bootloader никогда не читает (это делает `app` при
// старте), так что причина падения bootloader'а в release лежит в PANIC до
// первого удачного старта приложения или до `cargo xtask panic-info`.
#[path = "../../app/src/panic.rs"]
mod panic;

#[cortex_m_rt::entry]
fn main() -> ! {
    let p = init_peripherals();
    let flash = Flash::new_blocking(p.FLASH);
{%- if signed == "true" %}
    // Новый образ проверяется ДО сторожа: проверка — один неделимый вызов
    // (сотня миллионов тактов на частоте после сброса), покормить сторож
    // внутри нечем. См. `screen`.
    let flash = screen(flash);
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
{%- if graph == "true" %}
    //
    // В release флеш обёрнут сторожем (см. [`Fed`]), запущенным здесь же —
    // до обмена разделов, но после проверки нового образа (если она есть).
    #[cfg(not(debug_assertions))]
    let flash = Mutex::new(RefCell::new(Fed::new(
        flash,
        IndependentWatchdog::new(p.{{watchdog_peripheral}}, WATCHDOG_TIMEOUT_US),
    )));
    #[cfg(debug_assertions)]
{%- endif %}
    let flash = Mutex::new(RefCell::new(flash));
    // `BOOTLOADER_STATE` — со страницей своего сектора, а не всего чипа (см.
    // `STATE_ERASE_SIZE`), но над тем же `Flash`: свой мьютекс, а не второй
    // объект контроллера.
    let state = Mutex::new(RefCell::new(Paged::<_, STATE_ERASE_SIZE>::new(
        Shared::new(&flash),
    )));

    let config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &state);
    let active_offset = config.active.offset();
    let bl = BootLoader::prepare::<_, _, _, {{write_size}}>(config);

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
    // поднять HAL и подтвердить образ.
    #[cfg(not(debug_assertions))]
    flash.lock(|flash| flash.borrow_mut().watchdog.pet());
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

/// Страница раздела `BOOTLOADER_STATE` — размер сектора, в котором он лежит.
///
/// Цельный `Flash` объявляет страницей самый крупный сектор чипа, а
/// `Partition::new` паникует на разделе, не кратном странице. На F2/F4/F7
/// `STATE` лежит в мелком секторе (у F4 — 16 KiB, а с разделом настроек
/// 64 KiB при странице 128 KiB), поэтому его страница своя. Число посчитано при
/// генерации тем же расчётом, что адреса в `memory.x`; то же самое стоит в
/// `bsp::ota` — разделу в приложении и в bootloader'е расходиться нельзя.
const STATE_ERASE_SIZE: usize = {{state_erase_size}};
{%- if signed == "true" %}

/// Проверяет новый образ в `DFU`, если обмен запрошен, и возвращает флеш
/// дальше — в обмен и прыжок.
///
/// Проверка подписи живёт здесь, а не в приложении: ключ зашит в bootloader,
/// который обновлением не перезаписывается, и проверяется именно то, что
/// сейчас станет `ACTIVE`, — ни ошибка в OTA-коде приложения, ни запись в
/// `DFU` в обход него не приведут к запуску неподписанного образа. Решает
/// `adapters::ota::screen_update` (там же — почему проверяется только новый
/// запрос, а откат нет); отказ снимает запрос на обмен, и загружается прежний
/// образ. О неудаче отправитель узнаёт по тому, что версия после перезагрузки
/// не сменилась.
///
/// Свой мьютекс над `Flash`, а не тот, что дальше: сторож в release запускает
/// `Fed::new`, и проверка обязана закончиться раньше.
///
/// Причина отказа печатается строкой на вариант, а не через
/// `defmt::Debug2Format`: тот тянет `core::fmt` — несколько килобайт в зону
/// bootloader'а, где их и так впритык.
fn screen(
    flash: Flash<'static, embassy_stm32::flash::Blocking>,
) -> Flash<'static, embassy_stm32::flash::Blocking> {
    use adapters::ota::{Rejection, Screening};
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;

    let flash = Mutex::<NoopRawMutex, _>::new(RefCell::new(flash));
    {
        let state = Mutex::<NoopRawMutex, _>::new(RefCell::new(Paged::<_, STATE_ERASE_SIZE>::new(
            Shared::new(&flash),
        )));
        let mut config = BootLoaderConfig::from_linkerfile_blocking(&flash, &flash, &state);
        let capacity = config.active.size();
        let Ok(screening) = adapters::ota::screen_update(
            &mut config.dfu,
            &mut config.state,
            capacity,
            &OTA_PUBLIC_KEY_IN_IMAGE,
        ) else {
            // Флеш не читается — дальше обмен всё равно упал бы; паника хотя
            // бы оставляет причину в PANIC (`cargo xtask panic-info`).
            panic!("boot: проверка нового образа: ошибка флеша");
        };
        match screening {
            Screening::NothingToCheck => {}
            Screening::Approved => info!("boot: подпись нового образа сошлась"),
            Screening::Rejected(why) => defmt::warn!(
                "boot: новый образ отвергнут ({=str}), загружается прежний",
                match why {
                    Rejection::NoRecord => "нет записи о подписи",
                    Rejection::Length => "негодная длина",
                    Rejection::NoPublicKey => "ключ не подставлен",
                    Rejection::Signature => "подпись не сошлась",
                }
            ),
        }
    }
    flash.into_inner().into_inner()
}

/// Открытый ключ, которым проверяется подпись нового образа.
///
/// Не константа в исходнике, а файл `ota-public-key.bin` в корне проекта: его
/// создаёт `cargo xtask build` вместе с закрытым и приносит сюда `build.rs`
/// через `OUT_DIR`. Нули означают «ключ ещё не создан» (файла нет), и
/// `screen_update` отвергает с ними любой образ.
///
/// Статик с именем, а не `const`: `cargo xtask build` находит его в ELF
/// bootloader'а и сверяет с ключом, которым подписывает образ. `#[used]` и
/// `no_mangle` вдвоём: первое просит оставить статик, второе — сохранить имя.
#[used]
#[expect(unsafe_code, reason = "обоснование — в комментарии выше")] // имя символа нужно xtask, см. выше
#[unsafe(no_mangle)]
pub static OTA_PUBLIC_KEY_IN_IMAGE: [u8; 32] =
    *include_bytes!(concat!(env!("OUT_DIR"), "/ota-public-key.bin"));
{%- endif %}
{%- if graph == "true" %}

/// Таймаут сторожа — то же число из `[env]` crates-cross/.cargo/config.toml,
/// что у приложения (`bsp::wdg::HW_TIMEOUT_US`, там же и почему одно).
#[cfg(not(debug_assertions))]
const WATCHDOG_TIMEOUT_US: u32 = match u32::from_str_radix(env!("WATCHDOG_TIMEOUT_US"), 10) {
    Ok(us) => us,
    Err(_) => panic!("WATCHDOG_TIMEOUT_US в .cargo/config.toml — не число"),
};

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

    /// Диапазон не дробится: `embassy-boot` при обмене стирает по одной
    /// странице (крупнейший сектор чипа) за вызов, а дробить по `ERASE_SIZE`
    /// здесь нельзя — на F4/F7/H7 секторы неравные, и граница посреди
    /// настоящего сектора дала бы отказ стирания. Раздел `BOOTLOADER_STATE`
    /// (стирается целиком после отката) приходит сюда же — через
    /// `Paged`→`Shared`, — и он не крупнее страницы обмена: на F4 это сектор
    /// 3 (16 KiB) или, с разделом настроек, сектор 4 (64 KiB), на
    /// равносекторных чипах — несколько страниц по паре килобайт. Таймаут
    /// сторожа должен пережить стирание самого крупного из них.
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
