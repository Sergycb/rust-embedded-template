#![doc = include_str!("../../../docs/modules/cpu.md")]
//! Чип: единственная точка, где поднимается HAL, и [`Cpu`] — всё, что чип
//! отдаёт плате одним объектом.

use core::marker::PhantomData;
{%- if bank_switch != "" %}

use crate::banks;
{%- endif %}
{%- if cache != "" %}

use crate::cache;
{%- endif %}
{%- if cpu_freq_boost == "true" %}

use crate::clocks;
{%- endif %}
{%- if has_resources == "true" %}

// Типы групп `wiring.rs`: `split_resources!` раскрывается в них по именам, а
// перечислять группы поимённо — значит править этот список с каждой новой.
#[expect(
    clippy::wildcard_imports,
    reason = "`split_resources!` раскрывается в типы групп по именам"
)]
use crate::wiring::*;
{%- endif %}

use embassy_sync::blocking_mutex::raw::{RawMutex, ThreadModeRawMutex};

/// Что прошивка знает о чипе, на котором запущена.
///
/// Данные, а не порт: поведения, которое стоило бы подменять в тестах, у них
/// нет, а домен ими не пользуется — читают их `app` (лог старта, сведения об
/// устройстве) и target-тесты. Поэтому тип здесь, рядом с [`Cpu`], который его
/// заполняет, а не в `ports`.
///
/// Причины сброса здесь нет: флаги `RCC` читает `main` приложения
/// (`cpu::system::System::take_cause`) раньше, чем строится плата. Частота
/// одна — `sys`: её заполняют все семейные реализации `rcc`, остальной состав
/// `Clocks` генерируется под чип и в шаблоне неизвестен.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub struct CpuInfo {
    /// Имя чипа, выбранного при генерации проекта (`{{chip}}`).
    pub chip: &'static str,
    /// Уникальный 96-битный номер экземпляра чипа — заводской, не меняется.
    pub uid: [u8; 12],
    /// Объём flash чипа в байтах — весь, а не раздел приложения.
    pub flash_size: u32,
    /// Частота ядра (`sys`), Гц; `None`, если HAL её не посчитал.
    pub sys_hz: Option<u32>,
}

/// Чип и вся его периферия — то, что получает [`crate::init`].
///
/// `M` — метка вызывающего: `app` и `boot` задают её одним alias. Свои замки
/// (`crate::flash::Lock`, `crate::external::ExternalMutex`) крейт заводит на конкретном
/// `ThreadModeRawMutex`: они живут в статиках этого крейта, и тип у них один на
/// обоих потребителей, а не параметр.
pub struct Cpu<M: RawMutex> {
    info: CpuInfo,
    flash: crate::flash::Flash,
{%- if external_flash == "true" %}
    ext_flash: crate::external::ExtFlash,
{%- endif %}
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- unless f[0] == "tcp" or f[0] == "udp" %}
{%- assign parts = f[4] | split: "_" %}
{%- if f[0] == "uart" %}
    {{ f[4] }}: crate::com::{{ f[4] }}::Serial,
{%- elsif f[0] == "can" %}
    {{ f[4] }}: crate::can::{{ f[4] }}::Can,
{%- else %}
    {{ f[4] }}: crate::usb::Usb,
{%- endif %}
{%- endunless %}
{%- endfor %}
{%- if has_net == "true" %}
    ethernet: crate::eth::Ethernet,
    seed: u64,
{%- endif %}
    system: crate::system::System,
{%- if graph == "true" %}
    watchdog: crate::wdg::UnarmedBoardWatchdog,
{%- endif %}
    _mutex: PhantomData<M>,
}

// Вручную: у драйверов `embassy-stm32` `Debug` есть не у всех, а печатать
// целый чип незачем — сведения о нём лежат в `info`.
impl<M: RawMutex> core::fmt::Debug for Cpu<M> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Cpu")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl<M: RawMutex> ports::Cpu for Cpu<M> {
    type CpuInfo = CpuInfo;
    // Свои замки крейта — на `ThreadModeRawMutex`: тип у них один на `app` и
    // `boot`, а `M` — метка вызывающего (см. `Cpu`).
    type Mutex = ThreadModeRawMutex;
    type Flash = crate::flash::Flash;
{%- if external_flash == "true" %}
    type ExtFlashBus = crate::external::ExtFlash;
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- unless f[0] == "tcp" or f[0] == "udp" %}
{%- assign parts = f[4] | split: "_" %}
{%- if f[0] == "uart" %}
    type {% for part in parts %}{{ part | capitalize }}{% endfor %} = crate::com::{{ f[4] }}::Serial;
{%- elsif f[0] == "can" %}
    type {% for part in parts %}{{ part | capitalize }}{% endfor %} = crate::can::{{ f[4] }}::Can;
{%- else %}
    type {% for part in parts %}{{ part | capitalize }}{% endfor %} = crate::usb::Usb;
{%- endif %}
{%- endunless %}
{%- endfor %}
{%- if has_net == "true" %}
    type Ethernet = crate::eth::Ethernet;
{%- endif %}
    type System = crate::system::System;
{%- if graph == "true" %}
    type Watchdog = crate::wdg::UnarmedBoardWatchdog;
{%- endif %}

    const WATCHDOG_MIN_TIMEOUT_US: u64 = {% if graph == "true" %}crate::wdg::HW_TIMEOUT_MIN_US{% else %}0{% endif %};

    fn uid(&self) -> [u8; 12] {
        self.info.uid
    }

    #[allow(
        clippy::type_complexity,
        reason = "кортеж — это и есть весь набор периферии чипа: по типу на рычаг"
    )]
    fn split(
        self,
    ) -> (
        // По рычагу на строку: их набор зависит от генерации.
        Self::CpuInfo,
        Self::Flash,
{%- if external_flash == "true" %}
        Self::ExtFlashBus,
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- unless f[0] == "tcp" or f[0] == "udp" %}
{%- assign parts = f[4] | split: "_" %}
        Self::{% for part in parts %}{{ part | capitalize }}{% endfor %},
{%- endunless %}
{%- endfor %}
{%- if has_net == "true" %}
        Self::Ethernet,
{%- endif %}
        Self::System,
{%- if graph == "true" %}
        Self::Watchdog,
{%- endif %}
{%- if has_net == "true" %}
        u64,
{%- endif %}
    ) {
        (
            // По рычагу на строку: их набор зависит от генерации.
            self.info,
            self.flash,
{%- if external_flash == "true" %}
            self.ext_flash,
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- unless f[0] == "tcp" or f[0] == "udp" %}
            self.{{ f[4] }},
{%- endunless %}
{%- endfor %}
{%- if has_net == "true" %}
            self.ethernet,
{%- endif %}
            self.system,
{%- if graph == "true" %}
            self.watchdog,
{%- endif %}
{%- if has_net == "true" %}
            self.seed,
{%- endif %}
        )
    }
}

/// Поднимает чип и отдаёт его периферию: HAL, кэши, распиновку.
///
/// Больше здесь ничего не происходит — ни сторож не запускается, ни драйверы
/// не работают: [`Cpu`] объекты создаёт, а распоряжается ими приложение.
/// Синхронный, потому что его зовёт и `boot`, у которого исполнителя нет:
/// внешняя флешь не опознаётся здесь — это отдельный [`crate::external::init`].
///
/// Зовётся ровно один раз за запуск, и это не соглашение, а свойство:
/// внутри `embassy_stm32::init`, который паникует «init called more than
/// once!» при втором вызове.
///
/// # Panics
///
/// Если `embassy_stm32::init` вызван второй раз за запуск.
#[must_use]
pub fn init() -> Cpu<ThreadModeRawMutex> {
    init_with()
}

/// [`init`] с меткой мьютекса вызывающего (`app`/`boot` задают её одним alias).
///
/// Свои замки крейт заводит на `ThreadModeRawMutex` — см. [`Cpu`].
{%- if external_flash == "true" %}
///
/// # Panics
///
/// Если HAL не посчитал частоту `sys`: задержкам драйвера внешней флеши нужна
/// настоящая частота, а не догадка.
{%- endif %}
#[must_use]
pub fn init_with<M: RawMutex>() -> Cpu<M> {
{%- if bank_switch != "" %}
    // До HAL и до любой работы с флешем: раскладка однобанковая, а с завода
    // чип двухбанковый (см. `crate::banks`).{% if ota == "true" %} На новом кристалле переключит
    // bootloader: он стартует первым.{% endif %}
    banks::ensure_single_bank();
{%- endif %}
{%- if cpu_freq_boost == "true" %}
    // До HAL: `init()` проверяет частоту ядра по option byte, а записанный
    // бит вступает в силу только сбросом (см. `crate::clocks`).
    clocks::ensure_cpu_freq_boost();
{%- endif %}
    let p = init_peripherals(config());
{%- if cache != "" %}
    // Сразу за HAL (латентность флеша и тактирование выставлены) и до всего
    // остального: кэши с завода выключены (см. `crate::cache`).
    cache::enable();
{%- endif %}
    // Единственный способ узнать фактическое тактирование: `Clocks`
    // заполняется внутри `init_peripherals`, наружу отдаётся только так.
    // Дамп в лог — не отладочный мусор, а штатная сводка старта: по нему
    // видно, что получили делители. Включите в `config()` кварц
    // (`rcc.hse`) и он не заведётся — до дампа не дойдёт: `embassy_stm32::init`
    // ждёт `HSERDY` без таймаута и без отката на HSI, и плата висит молча{% if ota == "true" and graph == "true" %}
    // (в release её сбросит сторож, запущенный bootloader-ом){% endif %}.
    // В `CpuInfo` уходит только `sys` числом: целый `Clocks` — тип HAL,
    // генерируется под чип.
    //
    // Частота блока, под который `cpu` собирает драйвер (UART, CAN…):
    // `defmt::info!("uart clk {}", rcc::frequency::<peripherals::USART1>().0)`
    // — она берётся не с шины, а через мультиплексор (`Config.rcc.mux`),
    // и от общего дампа может отличаться.
    let clocks = embassy_stm32::rcc::clocks(&p.RCC);
    defmt::info!("cpu: чип готов, частоты {}", clocks);
    let sys_hz = clocks.sys.to_hertz().map(|hz| hz.0);
    let uid = embassy_stm32::uid::uid();

    // Контроллер флеша забирается до `split_resources!`: группы распиновки
    // (`crate::wiring`) флеш не содержат — им распоряжается сам `cpu`.
    let flash = crate::flash::new(p.FLASH);
{%- if has_resources == "true" %}

    // Вся остальная периферия разбирается здесь, по группам `wiring.rs`
    // (docs/resources.md), и каждая группа уходит в конструктор своего
    // модуля. Выводы и DMA-каналы платы добавляются в группы — наружу из
    // `cpu` по-прежнему уходит не периферия, а готовый объект под трейтом.
    let r = split_resources!(p);
{%- endif %}
{%- if external_flash == "true" %}
    // Внешняя флеш — одна на {% if ota == "true" and ota_store == "external" %}`STATE`/`DFU`{% if config_store == "external" %} и настройки{% endif %}{% else %}настройки{% endif %}: драйвер
    // собирается здесь же, из группы шины; микросхему опознаёт
    // `crate::external::init`, когда его позовёт `app`.
    //
    // `sys` заполняет любая семейная реализация `rcc`; без частоты задержки
    // драйвера ждали бы неверно.
    let ext_flash = crate::external::new(r.ext_flash, sys_hz.expect("HAL не посчитал частоту sys"));
{%- endif %}
{%- if has_net == "true" %}
{%- if rng_irq != "" %}
    let seed = crate::rng::seed(r.rng);
{%- else %}
    let seed = crate::eth::seed_from_uid(uid);
{%- endif %}
    let ethernet = crate::eth::new(uid);
{%- endif %}
{%- if graph == "true" %}
    let watchdog = crate::wdg::UnarmedBoardWatchdog::new(r.watchdog);
{%- endif %}

    // `usize` на Cortex-M — 32 бита, усечения нет.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "`usize` на Cortex-M — 32 бита"
    )]
    let flash_size = embassy_stm32::flash::FLASH_SIZE as u32;

    Cpu {
        info: CpuInfo {
            chip: "{{chip}}",
            uid,
            flash_size,
            sys_hz,
        },
        flash,
{%- if external_flash == "true" %}
        ext_flash,
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- unless f[0] == "tcp" or f[0] == "udp" %}
{%- if f[0] == "uart" %}
        {{ f[4] }}: crate::com::{{ f[4] }}::new(r.{{ f[4] }}),
{%- elsif f[0] == "can" %}
        {{ f[4] }}: crate::can::{{ f[4] }}::new(r.{{ f[4] }}),
{%- else %}
        {{ f[4] }}: crate::usb::new_{{ f[4] }}(r.{{ f[4] }}),
{%- endif %}
{%- endunless %}
{%- endfor %}
{%- if has_net == "true" %}
        ethernet,
        seed,
{%- endif %}
        system: crate::system::System,
{%- if graph == "true" %}
        watchdog,
{%- endif %}
        _mutex: PhantomData,
    }
}

{%- if dual_core == "true" %}

// Двухъядерный чип (chip_feature вида "...-cm7"/"...-cm4") прячет
// `embassy_stm32::init()` за `#[cfg(not(feature = "_dual-core"))]` — вместо
// него только `init_primary()`/`init_secondary()`, координируемые через
// `SharedData` по общему адресу в обеих прошивках. Здесь второй прошивки
// нет: шаблон использует ТОЛЬКО то ядро, что выбрано в каскаде, как
// единственное активное — `SharedData` объявлена локально и никуда не
// публикуется, взаимодействие с фактическим вторым ядром не оркеструется.
// На STM32H745/747/755/757 (в отличие от STM32WL54/55) это НЕ значит, что
// второе ядро выключено: с заводскими option bytes (BCM7 = BCM4 = 1) оба
// стартуют при сбросе — CM7 с 0x0800_0000, CM4 с 0x0810_0000 (RM0399
// Table 20). Раскладка `memory.x` место старта второго ядра не занимает, но
// само ядро это не останавливает: бит второго ядра (BCM4 для CM7, BCM7 для
// CM4) снимают в option bytes руками — docs/flash.md, «Память второго
// ядра»; шаблон их не трогает. Полноценная AMP-поддержка — осознанно не
// входит в шаблон.
/// Поднимает HAL на двухъядерном чипе: выбранное ядро — ведущее.
pub(crate) fn init_peripherals(config: embassy_stm32::Config) -> embassy_stm32::Peripherals {
    static SHARED_DATA: core::mem::MaybeUninit<embassy_stm32::SharedData> =
        core::mem::MaybeUninit::uninit();
    embassy_stm32::init_primary(config, &SHARED_DATA)
}
{%- else %}

/// Поднимает HAL — то, с чего начинается [`init_with`].
pub(crate) fn init_peripherals(config: embassy_stm32::Config) -> embassy_stm32::Peripherals {
    embassy_stm32::init(config)
}
{%- endif %}

/// Конфиг HAL: умолчания `embassy-stm32`{% if has_net == "true" %}{% if rng_clock != "" and rng_clock != "default" %} и клок блока `RNG` для сетевого
/// стека, которого они не дают (`docs/modules/cpu-rng.md`){% endif %}{% endif %}.
fn config() -> embassy_stm32::Config {
{%- if has_net != "true" %}
    embassy_stm32::Config::default()
{%- elsif rng_clock == "pll48" %}
    let mut config = embassy_stm32::Config::default();
    // RNG — от PLL48CK, выхода Q основного PLL, а по умолчанию PLL выключен:
    // HSI 16 МГц / 8 × 96 = 192 МГц на VCO, / 4 = 48 МГц. Ядро остаётся на HSI.
    config.rcc.pll = Some(embassy_stm32::rcc::Pll {
        prediv: embassy_stm32::rcc::PllPreDiv::DIV8,
        mul: embassy_stm32::rcc::PllMul::MUL96,
        divp: None,
        divq: Some(embassy_stm32::rcc::PllQDiv::DIV4),
        divr: None,
    });
    config
{%- elsif rng_clock == "rngsel" or rng_clock == "clk48sel" %}
    let mut config = embassy_stm32::Config::default();
    // Мультиплексор по умолчанию — ноль, а ноль у этой линии — клок, которого
    // нет: без него `Rng::new` не заведётся.
    config.rcc.mux.{{rng_clock}} = embassy_stm32::rcc::mux::{% if rng_clock == "rngsel" %}Rngsel{% else %}Clk48sel{% endif %}::{{rng_source}};
    config
{%- else %}
    embassy_stm32::Config::default()
{%- endif %}
}
