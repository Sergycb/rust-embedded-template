#![no_std]
#![no_main]

use core::cell::RefCell;
{% if ota_store == "external" %}
use adapters::ota::BootPlan;
{%- else %}
use adapters::flash::{Paged, Shared};
{%- endif %}
use defmt::info;
// RTT — дефолтный транспорт в обоих профилях (см. crates-cross/app/src/main.rs);
// boot вдобавок не спавнит embassy-задач, поэтому очередь+drain-таск под
// USB/UART (см. graph.rs) сюда в принципе не встраивается —
// одну диагностическую строку перед прыжком не стоит того усложнять.
use defmt_rtt as _;
{%- if ota_store == "external" %}
use embassy_boot_stm32::State;
{%- endif %}
use embassy_boot_stm32::{BootLoader, BootLoaderConfig};
use embassy_stm32::flash::{FLASH_BASE, Flash};
{%- if graph == "true" %}
#[cfg(not(debug_assertions))]
use embassy_stm32::wdg::IndependentWatchdog;
{%- endif %}
use embassy_sync::blocking_mutex::Mutex;
{%- if ota_store == "external" %}
use embedded_storage::nor_flash::NorFlash;
{%- endif %}
{%- if graph == "true" %}
#[cfg(not(debug_assertions))]
use embedded_storage::nor_flash::{ErrorType, {% if ota_store != "external" %}NorFlash, {% endif %}ReadNorFlash};
{%- endif %}

// Паникёр по профилю — тот же исходник, что у `app`: модуль подключён по
// пути, а не копией и не общим крейтом (boot намеренно не зависит ни от
// `bsp`, ни от `app`). Дамп bootloader никогда не читает (это делает `app` при
// старте), так что причина падения bootloader'а в release лежит в PANIC до
// первого удачного старта приложения или до `cargo xtask panic-info`.
#[path = "../../app/src/panic.rs"]
mod panic;
{%- if bank_switch != "" %}

// Переключение флеша в одобанковый режим — тот же исходник, что у `bsp`, по той
// же причине, что и паникёр: bootloader не зависит от `bsp`.
#[path = "../../bsp/src/banks.rs"]
mod banks;
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
    let external: ExternalFlash = adapters::flash::Unconnected;
{%- endif %}
{%- if signed == "true" %}
    // Новый образ проверяется ДО сторожа: проверка — один неделимый вызов
    // (сотня миллионов тактов на частоте после сброса), покормить сторож
    // внутри нечем. См. `screen`.
{%- if ota_store == "external" %}
    let (flash, external, swap_allowed) = screen(flash, external);
{%- elsif dfu_bank == "true" %}
    let (flash, dfu) = screen(flash, dfu);
{%- else %}
    let flash = screen(flash);
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
{%- if graph == "true" %}
    //
    // В release флеш{% if ota_store == "external" %} — оба, внутренний и внешний, —{% endif %} обёрнут сторожем (см. [`Fed`]),
    // запущенным здесь же — до обмена разделов, но после проверки нового
    // образа (если она есть).
    #[cfg(not(debug_assertions))]
    let watchdog = {
        let mut watchdog = IndependentWatchdog::new(p.{{watchdog_peripheral}}, WATCHDOG_TIMEOUT_US);
        watchdog.unleash();
        RefCell::new(watchdog)
    };
    #[cfg(not(debug_assertions))]
    let flash = Fed::new(flash, &watchdog);
{%- if ota_store == "external" %}
    #[cfg(not(debug_assertions))]
    let external = Fed::new(external, &watchdog);
{%- endif %}
{%- if dfu_bank == "true" %}
    #[cfg(not(debug_assertions))]
    let dfu = Fed::new(dfu, &watchdog);
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
        prepare(config)
    } else {
        BootLoader { state: State::Boot }
    };
{%- else %}
    let bl = prepare(config);
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
{%- else %}

/// Драйвер внешней флеши, на которой лежат `STATE` и `DFU`, — замените на
/// свой: любой блокирующий `embedded_storage::nor_flash::NorFlash`. Та же
/// микросхема, что у приложения (`bsp::external`), но драйвер блокирующий:
/// bootloader не поднимает исполнитель.
type ExternalFlash = adapters::flash::Unconnected;

/// Страница обмена, под которую посчитана раскладка (`memory.x`): `ACTIVE`
/// кратен ей. Обмен идёт страницами `max(ACTIVE::ERASE_SIZE,
/// DFU::ERASE_SIZE)`, и драйвер с более крупным стиранием потребовал бы
/// `ACTIVE`, кратного уже его странице, — `assert_partitions` паниковал бы на
/// старте. Поэтому стирание драйвера обязано эту страницу делить.
const EXT_PAGE_SIZE: usize = {{ext_page_size}};

const _: () = assert!(
    EXT_PAGE_SIZE.is_multiple_of(<ExternalFlash as NorFlash>::ERASE_SIZE),
    "стирание драйвера внешней флеши не делит страницу обмена (EXT_PAGE_SIZE): \
     перегенерируйте раскладку или возьмите стирание мельче (сектор 4 KiB)"
);

/// Журнал `STATE` в раскладке посчитан на слово не шире 32 байт — столько же
/// держит буфер адаптера в приложении (`adapters::ota::MAX_WRITE_SIZE`).
const _: () = assert!(
    <ExternalFlash as NorFlash>::WRITE_SIZE <= adapters::ota::MAX_WRITE_SIZE,
    "слово записи драйвера внешней флеши шире 32 байт"
);

/// Буфер обмена: кратен слову записи и внутреннего флеша (`ACTIVE`), и
/// внешнего (`DFU`), и не меньше слова `STATE` — требования `prepare_boot`.
/// Оба слова — степени двойки, так что общее кратное — большее из них.
const BUFFER_SIZE: usize = if <ExternalFlash as NorFlash>::WRITE_SIZE > {{write_size}} {
    <ExternalFlash as NorFlash>::WRITE_SIZE
} else {
    {{write_size}}
};

/// Обмен разделов, если он запрошен, — или текущий образ, если внешняя флеш
/// не отвечает, а обмен не начат. Решает `adapters::ota::boot_plan` (там же
/// почему и host-тест с прерванным обменом): `Swap` — строгий
/// `BootLoader::prepare`, ошибка которого — паника, сброс и продолжение обмена
/// по журналу, а не прыжок в наполовину переписанный `ACTIVE`.
fn prepare<ACTIVE: NorFlash, DFU: NorFlash, STATE: NorFlash>(
    mut config: BootLoaderConfig<ACTIVE, DFU, STATE>,
) -> BootLoader {
    match adapters::ota::boot_plan(&mut config.state) {
        BootPlan::Swap => BootLoader::prepare::<_, _, _, BUFFER_SIZE>(config),
        BootPlan::Boot => {
            BootLoader::try_prepare::<_, _, _, BUFFER_SIZE>(config).unwrap_or_else(|_| {
                defmt::warn!("boot: внешняя флеш не отвечает — загружается текущий образ");
                BootLoader { state: State::Boot }
            })
        }
        BootPlan::BootCurrent => {
            defmt::warn!("boot: внешняя флеш не отвечает — загружается текущий образ");
            BootLoader { state: State::Boot }
        }
    }
}
{%- endif %}
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
{%- if ota_store == "external" %}
///
/// Третье в ответе — можно ли обменивать разделы. Нельзя, если внешняя флеш
/// не ответила: образ не проверен, и обмен без проверки запустил бы
/// неподписанный — даже если на следующем шаге флеш вдруг ответит. Тогда
/// загружается текущий образ, запрос на обмен остаётся до следующего сброса.
fn screen(
    flash: Flash<'static, embassy_stm32::flash::Blocking>,
    external: ExternalFlash,
) -> (
    Flash<'static, embassy_stm32::flash::Blocking>,
    ExternalFlash,
    bool,
) {
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;

    let flash = Mutex::<NoopRawMutex, _>::new(RefCell::new(flash));
    let external = Mutex::<NoopRawMutex, _>::new(RefCell::new(external));
    let swap_allowed = {
        let mut config = BootLoaderConfig::from_linkerfile_blocking(&flash, &external, &external);
        let capacity = config.active.size();
        if let Ok(screening) = adapters::ota::screen_update(
            &mut config.dfu,
            &mut config.state,
            capacity,
            &OTA_PUBLIC_KEY_IN_IMAGE,
        ) {
            report(screening);
            true
        } else {
            defmt::warn!(
                "boot: внешняя флеш не отвечает — новый образ не проверен, обмен пропущен"
            );
            false
        }
    };
    (
        flash.into_inner().into_inner(),
        external.into_inner().into_inner(),
        swap_allowed,
    )
}
{%- elsif dfu_bank == "true" %}
fn screen(flash: Bank1, dfu: Bank2) -> (Bank1, Bank2) {
    use embassy_sync::blocking_mutex::raw::NoopRawMutex;

    let flash = Mutex::<NoopRawMutex, _>::new(RefCell::new(flash));
    let dfu = Mutex::<NoopRawMutex, _>::new(RefCell::new(dfu));
    {
        let state = Mutex::<NoopRawMutex, _>::new(RefCell::new(Paged::<_, STATE_ERASE_SIZE>::new(
            Shared::new(&flash),
        )));
        let mut config = BootLoaderConfig::from_linkerfile_blocking(&flash, &dfu, &state);
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
        report(screening);
    }
    (flash.into_inner().into_inner(), dfu.into_inner().into_inner())
}
{%- else %}
fn screen(
    flash: Flash<'static, embassy_stm32::flash::Blocking>,
) -> Flash<'static, embassy_stm32::flash::Blocking> {
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
        report(screening);
    }
    flash.into_inner().into_inner()
}
{%- endif %}

/// Исход проверки — в лог.
fn report(screening: adapters::ota::Screening) {
    use adapters::ota::{Rejection, Screening};

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
///
/// Сторож — по ссылке, а не полем: флешей под обменом может быть две
/// (внутренняя и внешняя, `ota=external`), а сторож один.
#[cfg(not(debug_assertions))]
struct Fed<'w, F> {
    flash: F,
    watchdog: &'w Watchdog,
}

/// Запущенный сторож, общий для всех [`Fed`].
#[cfg(not(debug_assertions))]
type Watchdog = RefCell<IndependentWatchdog<'static, embassy_stm32::peripherals::{{watchdog_peripheral}}>>;

#[cfg(not(debug_assertions))]
impl<'w, F> Fed<'w, F> {
    const fn new(flash: F, watchdog: &'w Watchdog) -> Self {
        Self { flash, watchdog }
    }
}

#[cfg(not(debug_assertions))]
impl<F: ErrorType> ErrorType for Fed<'_, F> {
    type Error = F::Error;
}

#[cfg(not(debug_assertions))]
impl<F: ReadNorFlash> ReadNorFlash for Fed<'_, F> {
    const READ_SIZE: usize = F::READ_SIZE;

    fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
        self.flash.read(offset, bytes)
    }

    fn capacity(&self) -> usize {
        self.flash.capacity()
    }
}

#[cfg(not(debug_assertions))]
impl<F: NorFlash> NorFlash for Fed<'_, F> {
    const WRITE_SIZE: usize = F::WRITE_SIZE;
    const ERASE_SIZE: usize = F::ERASE_SIZE;

    /// Длинный диапазон — по кускам до границы `ERASE_SIZE`, со сторожем перед
    /// каждым. `embassy-boot` стирает страницу обмена или весь
    /// `BOOTLOADER_STATE` (после отката) одним вызовом, и у внешней флеши со
    /// страницей 4 KiB это десятки команд стирания по 45–400 мс — дольше
    /// таймаута сторожа.
    ///
    /// Резать по самому `ERASE_SIZE` от начала диапазона было бы нельзя: у
    /// внутреннего флеша это крупнейший сектор, а на F4/F7/H7 секторы
    /// неравные, и кусок мог бы кончиться посреди настоящего сектора — отказ
    /// стирания. Граница же кратна `ERASE_SIZE` и потому всегда совпадает с
    /// границей сектора (они кратны своему размеру): у внутреннего флеша
    /// страница обмена и `STATE` в мелких секторах так и остаются одним
    /// куском. Таймаут сторожа должен пережить стирание одного куска — самого
    /// крупного сектора чипа.
    fn erase(&mut self, mut from: u32, to: u32) -> Result<(), Self::Error> {
        #[expect(
            clippy::cast_possible_truncation,
            reason = "`ERASE_SIZE` флеша — меньше 4 ГиБ"
        )]
        let page = F::ERASE_SIZE as u32;
        while from < to {
            let end = to.min((from / page + 1) * page);
            self.watchdog.borrow_mut().pet();
            self.flash.erase(from, end)?;
            from = end;
        }
        Ok(())
    }

    fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
        self.watchdog.borrow_mut().pet();
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
