#![no_std]
#![no_main]
{%- assign xs = links | split: "," %}
{%- if ota_store == "external" %}

mod external;
{%- endif %}

mod layout;
mod signature;

use defmt::info;
// RTT — дефолтный транспорт в обоих профилях (см. crates-cross/app/src/panic.rs);
// почему без UART-транспорта — crates-cross/boot/Cargo.toml.
use defmt_rtt as _;
use embassy_boot_stm32::{BootLoader, BootLoaderConfig};
{%- if ota_store == "external" %}

use embassy_boot_stm32::State;
{%- endif %}
{%- if ota_store == "external" or boot_partitioned == "true" %}

use core::cell::RefCell;
{%- endif %}

use embassy_embedded_hal::flash::partition::BlockingPartition;
{%- if ota_store == "external" or boot_partitioned == "true" %}

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::ThreadModeRawMutex;
{%- endif %}
{%- if state_paged == "true" %}

use cpu::flash::STATE_ERASE_SIZE;
{%- endif %}
{%- if swap_paged == "true" and dfu_bank != "true" %}

use cpu::flash::SWAP_PAGE_SIZE;
{%- endif %}
{%- if boot_partitioned == "true" %}

use flash_page::ErasePage;
{%- endif %}
use ports::Cpu as _;
{%- if ota_store != "external" %}

/// Буфер обмена `BootLoader::prepare` — слово записи флеша из генерации
/// (`swap_buffer`, его можно переопределить `--define`). `prepare` требует,
/// чтобы буфер был кратен слову записи реального флеша; несовпадение — ошибка
/// сборки, а не паника на старте bootloader'а.
const SWAP_BUFFER_SIZE: usize = {{swap_buffer}};

const _: () = assert!(
    SWAP_BUFFER_SIZE.is_multiple_of(cpu::flash::WRITE_SIZE),
    "swap_buffer из генерации не кратен слову записи флеша чипа (cpu::flash::WRITE_SIZE): \
     пересоберите проект без --define swap_buffer или поправьте значение"
);
{%- endif %}
{%- if boot_partitioned == "true" %}

/// Весь флеш чипа как раздел: смещения те же, что у самого флеша, а операции
/// идут под замком контроллера.
type Whole = BlockingPartition<'static, ThreadModeRawMutex, cpu::flash::ChipFlash>;
{%- endif %}
{%- if boot_partitioned == "true" %}

/// Весь флеш чипа как раздел — основа разделов со своей страницей.
fn whole(flash: cpu::flash::Flash) -> Whole {
    BlockingPartition::new(flash.lock, 0, cpu::flash::SIZE)
}
{%- endif %}
{%- if swap_paged == "true" and dfu_bank != "true" %}

/// `ACTIVE` и `DFU` — со страницей обмена, крупнее физической.
fn swap_page(flash: cpu::flash::Flash) -> ErasePage<Whole, SWAP_PAGE_SIZE> {
    ErasePage::new(whole(flash))
}
{%- endif %}
{%- if state_paged == "true" %}

/// `BOOTLOADER_STATE` — со страницей своего сектора.
fn state_page(flash: cpu::flash::Flash) -> ErasePage<Whole, STATE_ERASE_SIZE> {
    ErasePage::new(whole(flash))
}
{%- endif %}

#[cortex_m_rt::entry]
fn main() -> ! {
    // Чип поднимает `cpu::init` — то же, что у приложения: банки, option byte,
    // HAL, кэши. Периферия транспортов и сеть bootloader'у не нужны: их отдаёт
    // `split`, а связываются они с `_`.
    let (
        // Периферия транспортов, Ethernet и затравка сети bootloader'у не нужны.
        _info,
        flash,
{%- if external_flash == "true" %}
        ext,
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- unless f[0] == "tcp" or f[0] == "udp" %}
        _,
{%- endunless %}
{%- endfor %}
{%- if has_net == "true" %}
        _ethernet,
{%- endif %}
        _system,
{%- if graph == "true" %}
        watchdog,
{%- endif %}
{%- if has_net == "true" %}
        _seed,
{%- endif %}
    ) = cpu::init().split();
{%- if external_flash == "true" %}
    // Микросхему опознаём до проверки подписи: `STATE` и `DFU` — на ней.
    cpu::external::init(ext, crate::layout::ext_flash_end());
{%- endif %}

{%- if ota_store == "external" %}
    // `STATE` и `DFU` — на внешней флеши: забираем опознанный драйвер из руки
    // чипа и держим в своём мьютексе — разделы делят один драйвер. `ACTIVE` —
    // во внутреннем флеше, его раздел — из ручки как есть.
    let state = external::take(ext);
    let external = Mutex::<ThreadModeRawMutex, _>::new(RefCell::new(state));
    let (active_start, active_len) = crate::layout::active();
    let (dfu_start, dfu_len) = crate::layout::dfu();
    let (state_start, state_len) = crate::layout::state();
    let config = BootLoaderConfig {
        active: BlockingPartition::new(flash.lock, active_start, active_len),
        dfu: BlockingPartition::new(&external, dfu_start, dfu_len),
        state: BlockingPartition::new(&external, state_start, state_len),
    };
{%- else %}
{%- if swap_paged == "true" and dfu_bank != "true" %}
    // `ACTIVE` и `DFU` — со страницей обмена `SWAP_PAGE_SIZE`, крупнее физической
    // страницы флеша (см. её описание): один мьютекс на оба раздела — они
    // делят один контроллер.
    let swap = Mutex::<ThreadModeRawMutex, _>::new(RefCell::new(swap_page(flash)));
{%- endif %}
{%- if state_paged == "true" %}
    // `BOOTLOADER_STATE` — со страницей своего сектора, а не всего чипа (см.
    // `STATE_ERASE_SIZE`), но над тем же контроллером: свой мьютекс, а не
    // второй объект флеша.
    let state = Mutex::<ThreadModeRawMutex, _>::new(RefCell::new(state_page(flash)));
{%- endif %}
    let (active_start, active_len) = crate::layout::active();
    let (dfu_start, dfu_len) = crate::layout::dfu();
    let (state_start, state_len) = crate::layout::state();
    let config = BootLoaderConfig {
{%- if swap_paged == "true" and dfu_bank != "true" %}
        active: BlockingPartition::new(&swap, active_start, active_len),
{%- else %}
        active: BlockingPartition::new(flash.lock, active_start, active_len),
{%- endif %}
{%- if dfu_bank == "true" %}
        dfu: BlockingPartition::new(flash.bank2, dfu_start, dfu_len),
{%- elsif swap_paged == "true" %}
        dfu: BlockingPartition::new(&swap, dfu_start, dfu_len),
{%- else %}
        dfu: BlockingPartition::new(flash.lock, dfu_start, dfu_len),
{%- endif %}
{%- if state_paged == "true" %}
        state: BlockingPartition::new(&state, state_start, state_len),
{%- else %}
        state: BlockingPartition::new(flash.lock, state_start, state_len),
{%- endif %}
    };
{%- endif %}
    // Новый образ проверяется до обмена разделов: см. `signature::screen`.
{%- if ota_store == "external" %}
    let (config, swap_allowed) = signature::screen(config);
    let bl = if swap_allowed {
        external::prepare(config)
    } else {
        BootLoader { state: State::Boot }
    };
{%- else %}
    let config = signature::screen(config);
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
    let entry = cpu::flash::FLASH_BASE as u32 + crate::layout::active().0;
{%- if cache contains "scb_d" %}
    // Кэш данных ядра включил `cpu::init` — и здесь тоже, а он переживает
    // прыжок в приложение. Строки, прочитанные bootloader'ом до стирания
    // разделов, устарели бы для приложения: сбрасываем кэш в память и
    // обесцениваем. Инструкции обесценит сам `load` (VTOR и invalidate I-cache).
    #[expect(
        unsafe_code,
        reason = "периферия ядра берётся через `steal()`: после прыжка инвариантов Rust нет"
    )]
    {
        // SAFETY: дальше идёт прыжок в приложение; после него ни один инвариант
        // Rust этого образа не действует, конфликтовать за периферию ядра некому.
        let mut core = unsafe { cortex_m::Peripherals::steal() };
        core.SCB.clean_invalidate_dcache(&mut core.CPUID);
    }
{%- endif %}
    info!("boot: переход в приложение по адресу {:x}", entry);
{%- if graph == "true" %}
    // Сторож — последним, прямо перед прыжком, и целиком достаётся приложению:
    // до первого кормления из графа ему нужно поднять HAL и запустить граф.
    // Раньше он не нужен: бутлоадер защищает от неисправного образа (не
    // стартует, не кормит), а не от собственного обмена разделов, сколько бы
    // тот ни длился. Остановить его уже нельзя — приложение лишь подхватывает
    // идущий счётчик (`cpu::wdg`, `arm()`). Без фичи `no-watchdog`: ею `cargo
    // xtask test target` собирает bootloader под тесты на плате, где кормить
    // сторож некому.
    #[cfg(not(feature = "no-watchdog"))]
    let _armed = watchdog.arm();
    // С фичей сторож не взводится, но значение из `split` обязано быть
    // использовано — иначе предупреждение о неиспользованной переменной.
    #[cfg(feature = "no-watchdog")]
    let _ = watchdog;
{%- endif %}
    // SAFETY: `load` ставит VTOR на этот адрес, читает из образа по нему
    // начальный MSP (первое слово) и reset-вектор (второе) и прыгает — за этой
    // строкой не живёт ни один инвариант Rust, в том числе паникёр и таблица
    // векторов, на которых держится release-сборка.
    //
    // Предпосылки — раскладка и записанный образ, и обе сводятся к файлам:
    // * `entry` — это `FLASH_BASE` плюс смещение раздела ACTIVE, которое
    //   `crate::layout` берёт из символов `__bootloader_active_*`, то есть из
    //   `ORIGIN(ACTIVE)` в `crates-cross/boot/memory.x` (тот же адрес, что
    //   `ORIGIN(FLASH)` в `crates-cross/app/memory.x`); раскладку приложения с
    //   линковкой сверяют `cargo xtask build` и прошивка перед тестами на плате.
    // * первое и второе слова по этому адресу — MSP и reset-вектор образа,
    //   которые линкер положил в тот же раздел. Что bootloader их не
    //   проверяет — оговорка выше, про повреждённый образ.
    //
    // `#[expect(unsafe_code)]` — как и у остальных unsafe в `crates-cross`
    // (перечень и почему — `crates-cross/Cargo.toml`, `[workspace.lints.rust]`):
    // линт требует, чтобы каждое такое место было перечислено явно.
    #[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
    unsafe {
        bl.load(entry)
    }
}

/// Паникёр bootloader'а — без сообщений и без дампа: сброс МК.
///
/// Сброс, а не останов: на нём держится восстановление после сбоя флеша —
/// строгий `BootLoader::prepare` (и проверка подписи) паникует, следующий
/// старт продолжает обмен по журналу `STATE`. Остановленное ядро ждало бы
/// цикла питания: сторож до прыжка в приложение не запущен. Ни сообщения, ни
/// `panic-persist`: формат причины стоит зоны bootloader'а, а читать её
/// некому — дамп разбирает только `app`.
#[panic_handler]
fn panic(_: &core::panic::PanicInfo) -> ! {
    cpu::System.reset_now()
}
