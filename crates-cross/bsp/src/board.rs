//! Объекты платы: `Board` собирает их из периферии чипа и отдаёт приложению.
//!
//! Границу `bsp` → `app` пересекают только они — объекты, реализующие порты из
//! `ports`. Ни `Peripherals`, ни отдельные пины, ни периферия ядра наружу не
//! выходят: что не разобрано на объекты, то и не отдано (docs/architecture.md,
//! «`Board` отдаёт объекты, а не периферию»).

use defmt::info;
{%- if has_resources == "true" %}

// Типы групп `resources.rs`: `split_resources!` раскрывается в них по именам, а
// перечислять группы поимённо — значит править этот список с каждой новой.
#[expect(
    clippy::wildcard_imports,
    reason = "`split_resources!` раскрывается в типы групп по именам"
)]
use crate::resources::*;
{%- endif %}

/// Что прошивка знает о чипе, на котором запущена.
///
/// Данные, а не порт: поведения, которое стоило бы подменять в тестах, у них
/// нет, а домен ими не пользуется — читают их `app` (лог старта) и
/// target-тесты, оба и так зависят от `bsp`. Поэтому тип здесь, рядом с
/// [`Board`], который его заполняет, а не в `ports`.
///
/// Причины последнего сброса здесь нет: в `embassy-stm32` для неё нет API, а
/// регистр флагов у каждого семейства свой. Частота одна — `sys`: её
/// заполняют все семейные реализации `rcc`, остальной состав `Clocks`
/// генерируется под чип и в шаблоне неизвестен.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub struct ChipSpec {
    /// Имя чипа, выбранного при генерации проекта (`stm32f407ve`).
    pub chip: &'static str,
    /// Уникальный 96-битный номер экземпляра чипа — заводской, не меняется.
    pub uid: [u8; 12],
    /// Объём flash чипа в байтах — весь, а не раздел приложения.
    pub flash_size: u32,
    /// Частота ядра (`sys`), Гц; `None`, если HAL её не посчитал.
    pub sys_hz: Option<u32>,
}

/// Объекты этой платы — всё, что приложение о ней знает.
///
/// Каждое поле — адаптер, реализующий порт из `ports` (кроме сторожа, который
/// реализует `watchdog::HardwareWatchdog` — тот же приём границы, только трейт
/// приходит из крейта сторожа), или данные о чипе ([`ChipSpec`], поле
/// `spec`). Периферии в полях нет вовсе: разбирать её на объекты — работа
/// `bsp`, а `Peripherals`, пины и группы `assign_resources!` остаются внутри
/// [`Board::new`].
#[derive(Debug)]
pub struct Board {
    /// Сведения о чипе: имя, заводской номер, объём flash, частота `sys`.
    pub spec: ChipSpec,
{%- if ota == "true" %}
    /// Адаптер обновления прошивки — порт `FirmwareUpdate` поверх разделов
    /// `DFU`/`BOOTLOADER_STATE`, собранных из символов `memory.x`
    /// (`bsp::ota`). Алгоритм обновления над портом — в `domain::ota`.
    /// {% if graph == "true" %}Уезжает в узел `OTA` графа (`fragments:` в `crates-cross/app/src/graph.rs`){% else %}Уезжает в задачу узла, которую спавнит `main` (`domain::ota::task`){% endif %}.
    pub updater: crate::ota::Updater,
    /// Канал доставки образа — реализация `ports::ImageSource`; заглушка
    /// (`bsp::ota::Link`), пока проект не подставит свой транспорт.
    pub ota_link: crate::ota::Link,
{%- endif %}
{%- if config == "true" %}
    /// Настройки, переживающие перезапуск и обновление прошивки:
    /// `adapters::settings` поверх {% if config_store == "eeprom" %}внутренней EEPROM{% elsif config_store == "external" %}внешней флеши (драйвер — `bsp/src/{% if ota == "true" and ota_store == "external" %}external{% else %}config{% endif %}.rs`){% else %}раздела `CONFIG` во флеше{% endif %}.
    /// Формат значений выбирает проект.
    pub settings: crate::settings::Settings,
{%- endif %}
{%- if interface == "uart" %}
    /// UART под узлом протокола ({{protocol}}) — заглушка `bsp::serial::Serial`,
    /// пока проект не подставит драйвер своей платы (`docs/modules/bsp-serial.md`).
    /// Уезжает в узел `{% if protocol == "modbus" %}MODBUS{% else %}HOST{% endif %}` графа (`fragments:` в
    /// `crates-cross/app/src/graph.rs`).
    pub serial: crate::serial::Serial,
{%- endif %}
{%- if interface == "can" %}
    /// Шина CAN под узлом `CANOPEN` — заглушка `bsp::can::Can`, пока проект не
    /// подставит контроллер своей платы (`docs/modules/bsp-can.md`). Уезжает в
    /// узел `CANOPEN` графа (`fragments:` в `crates-cross/app/src/graph.rs`).
    pub can: crate::can::Can,
{%- endif %}
{%- if interface == "tcp" or interface == "udp" %}
    /// Сеть под протоколом: фоновая работа стека для узла `NET` и сам стек, на
    /// котором приложение открывает сокет протокола (`bsp::net`). Устройство — заглушка, пока проект не подставит
    /// Ethernet своей платы (`docs/modules/bsp-net.md`).
    pub net: crate::net::Net,
{%- endif %}
{%- if interface == "usb" %}
    /// USB под протоколом: устройство для узла `USB` и поток CDC (`bsp::usb`).
    /// Драйвер — заглушка, пока проект не подставит USB-блок своей платы
    /// (`docs/modules/bsp-usb.md`).
    pub usb: crate::usb::Usb,
{%- endif %}
{%- if graph == "true" %}
    /// Сторож платы — заряженный, но ещё не запущенный: его забирает
    /// инициализатор блока `watchdog:` в графе задач
    /// (`board.watchdog.arm()`).
    ///
    /// Запускается там, а не здесь, потому что с этого момента железо тикает,
    /// а кормит его только тикер графа: чем короче промежуток между двумя
    /// этими точками, тем меньше шансов уйти в перезагрузки молча. Короче
    /// всего он в прологе `spawn_all`. Забыть запуск нельзя — блок
    /// `watchdog:` требует тип, реализующий `HardwareWatchdog`, а он есть
    /// только у запущенного (`bsp::wdg`).
    pub watchdog: crate::wdg::UnarmedBoardWatchdog,
{%- endif %}
}

impl Board {
    /// Собирает объекты платы: поднимает HAL, разбирает периферию и отдаёт то,
    /// что за ней стоит.
    ///
    /// Больше здесь ничего не происходит — ни сторож не запускается, ни
    /// периферия не настраивается «на будущее»: `bsp` объекты создаёт, а
    /// распоряжается ими приложение.
    ///
    /// Зовётся ровно один раз за запуск, и это не соглашение, а свойство:
    /// внутри `embassy_stm32::init`, который паникует «init called more than
    /// once!» при втором вызове.
    // `Default` тут был бы ложью: конструктор не только не дёшев, он ещё и
    // одноразовый — `Default::default()` на второй вызов уронил бы прошивку.
    #[expect(
        clippy::new_without_default,
        reason = "конструктор одноразовый: `Default` на второй вызов уронил бы прошивку"
    )]
    #[must_use]
    pub fn new() -> Self {
{%- if bank_switch != "" %}
        // До HAL и до любой работы с флешем: раскладка одобанковая, а с завода
        // чип двухбанковый (см. `crate::banks`). На новом кристалле сюда доходит
        // приложение без bootloader'а (OTA нет) — иначе переключил бы он.
        crate::banks::ensure_single_bank();
{%- endif %}
{%- if cpu_freq_boost == "true" %}
        // До HAL: `init()` проверяет частоту ядра по option byte, а записанный
        // бит вступает в силу только сбросом (см. `crate::clocks`).
        crate::clocks::ensure_cpu_freq_boost();
{%- endif %}
        let p = init_peripherals();
        // Единственный способ узнать фактическое тактирование: `Clocks`
        // заполняется внутри `init_peripherals`, наружу отдаётся только так.
        // Дамп в лог — не отладочный мусор, а штатная сводка старта: по нему
        // видно, какой источник выиграл (HSE не завёлся — HAL молча уедет на
        // HSI) и что получили делители. В `ChipSpec` уходит только `sys` числом:
        // целый `Clocks` — тип HAL, генерируется под чип.
        //
        // Частота конкретного блока, когда `Board` начнёт отдавать периферию:
        // `defmt::info!("uart clk {}", rcc::frequency::<peripherals::USART1>().0)`
        // — она берётся не с шины, а через мультиплексор (`Config.rcc.mux`),
        // и от общего дампа может отличаться.
        let clocks = embassy_stm32::rcc::clocks(&p.RCC);
        info!("bsp: board initialized, clocks {}", clocks);

{%- if has_resources == "true" %}
        // Вся периферия разбирается здесь, по группам `resources.rs`
        // (docs/resources.md), и каждая группа уходит в конструктор своего
        // модуля. Выводы и DMA-каналы платы добавляются в группы — наружу из
        // `bsp` по-прежнему уходит не периферия, а готовый объект, поле
        // `Board`, реализующее порт из `ports`.
        let r = split_resources!(p);
{%- if shared_flash == "true" %}
        let {% if dfu_bank == "true" %}(flash, dfu_bank){% else %}flash{% endif %} = crate::flash::new(r.flash.flash);
{%- endif %}
{%- else %}
        // Место для распиновки платы: заполните группы в `resources.rs`
        // (docs/resources.md), добавьте `use crate::resources::*;` и `let r =
        // split_resources!(p);` и собирайте из `r` свои драйверы. Наружу из `bsp` уходит не
        // периферия, а готовый объект — поле `Board`, реализующее порт из
        // `ports`.
{%- endif %}
{%- if ota == "true" and ota_store == "external" %}
        // Внешняя флеш — одна на `STATE`/`DFU`{% if config_store == "external" %} и настройки{% endif %}: драйвер
        // собирается здесь же, из периферии (см. `crate::external`).
        let external = crate::external::new();
{%- endif %}
{%- if interface == "tcp" or interface == "udp" %}
{%- if rng_irq != "" %}
        let seed = crate::rng::seed(r.rng.rng);
{%- else %}
        let seed = adapters::net::seed_from_uid(embassy_stm32::uid::uid());
{%- endif %}
{%- endif %}

        let sys_hz = clocks.sys.to_hertz().map(|hz| hz.0);

        // `usize` на Cortex-M — 32 бита, усечения нет.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "`usize` на Cortex-M — 32 бита"
        )]
        let flash_size = embassy_stm32::flash::FLASH_SIZE as u32;

        Self {
            spec: ChipSpec {
                chip: "{{chip}}",
                uid: embassy_stm32::uid::uid(),
                flash_size,
                sys_hz,
            },
{%- if ota == "true" %}
            updater: crate::ota::updater({% if ota_store == "external" %}external{% elsif dfu_bank == "true" %}flash, dfu_bank{% else %}flash{% endif %}),
            ota_link: crate::ota::link(),
{%- endif %}
{%- if config == "true" %}
            settings: crate::settings::new({% if config_store != "external" %}flash{% elsif ota == "true" and ota_store == "external" %}external{% endif %}),
{%- endif %}
{%- if interface == "uart" %}
            serial: crate::serial::new(r.serial),
{%- endif %}
{%- if interface == "can" %}
            can: crate::can::new(r.can),
{%- endif %}
{%- if interface == "tcp" or interface == "udp" %}
            net: crate::net::new(embassy_stm32::uid::uid(), seed, r.eth),
{%- endif %}
{%- if interface == "usb" %}
            usb: crate::usb::new(r.usb),
{%- endif %}
{%- if graph == "true" %}
            watchdog: crate::wdg::UnarmedBoardWatchdog::new(r.watchdog.iwdg),
{%- endif %}
        }
    }
}

{%- if dual_core == "true" %}

// Двухъядерный чип (chip_feature вида "...-cm7"/"...-cm4") прячет
// `embassy_stm32::init()` за `#[cfg(not(feature = "_dual-core"))]` — вместо
// него только `init_primary()`/`init_secondary()`, координируемые через
// `SharedData` по общему адресу в обоих прошивках. Здесь второй прошивки
// нет: шаблон использует ТОЛЬКО то ядро, что выбрано в каскаде, как
// единственное активное — `SharedData` объявлена локально и никуда не
// публикуется, взаимодействие с фактическим вторым ядром не оркеструется.
// На STM32H745/747/755/757 (в отличие от STM32WL54/55) это НЕ значит, что
// второе ядро выключено — по умолчанию оно тоже стартует при сбросе, и
// нужна ручная проверка/настройка option bytes (BCM4) перед прошивкой, см.
// README, раздел про dual-core. Полноценная AMP-поддержка — осознанно не
// входит в шаблон.
fn init_peripherals() -> embassy_stm32::Peripherals {
    static SHARED_DATA: core::mem::MaybeUninit<embassy_stm32::SharedData> =
        core::mem::MaybeUninit::uninit();
    embassy_stm32::init_primary(config(), &SHARED_DATA)
}
{%- else %}

fn init_peripherals() -> embassy_stm32::Peripherals {
    embassy_stm32::init(config())
}
{%- endif %}

/// Конфиг HAL: умолчания `embassy-stm32`{% if interface == "tcp" or interface == "udp" %}{% if rng_clock != "" and rng_clock != "default" %} и клок блока `RNG` для сетевого
/// стека, которого они не дают (`docs/modules/bsp-rng.md`){% endif %}{% endif %}.
fn config() -> embassy_stm32::Config {
{%- if interface != "tcp" and interface != "udp" %}
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
