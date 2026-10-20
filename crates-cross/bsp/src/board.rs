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
pub struct Board {
    /// Сведения о чипе: имя, заводской номер, объём flash, частота `sys`.
    pub spec: ChipSpec,
{%- if core_baseline != "true" %}
    /// Счётчик тактов ядра (`bsp::cycles`) — реализация `ports::CycleCounter`.
    pub cycles: crate::cycles::Cycles,
{%- endif %}
{%- if ota == "true" %}
    /// Адаптер обновления (`bsp::ota`) — порт `FirmwareUpdate` поверх разделов
    /// `DFU`/`BOOTLOADER_STATE`, собранных из символов `memory.x`. Образ
    /// приходит командами от узла протокола; алгоритм обновления над портами —
    /// в `domain::ota`.
    /// {% if graph == "true" %}Уезжает в узел `OTA` графа (`fragments:` в `crates-cross/app/src/graph.rs`){% else %}Уезжает в задачу узла, которую спавнит `main` (`domain::ota::task`){% endif %}.
    pub ota: crate::ota::Updater,
{%- endif %}
{%- if config == "true" %}
    /// Настройки, переживающие перезапуск и обновление прошивки:
    /// `adapters::settings` поверх {% if config_store == "eeprom" %}внутренней EEPROM{% elsif config_store == "external" %}внешней флеши (драйвер — `bsp/src/{% if ota == "true" and ota_store == "external" %}external{% else %}config{% endif %}.rs`){% else %}раздела `CONFIG` во флеше{% endif %}.
    /// Формат значений выбирает проект.
    pub settings: crate::settings::Settings,
{%- endif %}
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "modbus" or f[3] == "protobuf" %}
    /// Транспорт `{{ f[4] }}` протокола {{ f[3] }} поверх {% if f[0] == "uart" %}UART `{{ f[2] }}`{% elsif f[0] == "tcp" %}TCP{% elsif f[0] == "usb" %}USB CDC `{{ f[2] }}`{% else %}UDP{% endif %} платы, готовый к
    /// работе: кадрирование, буферы и сокет заведены в `bsp::link::{{ f[4] }}`. Уезжает в узел
    /// `{% if f[3] == "modbus" %}MODBUS{% else %}HOST{% endif %}_{{ f[4] | upcase }}`: в граф (`fragments:` в `crates-cross/app/src/graph.rs`) или, при
    /// `graph=no`, в задачу, которую спавнит `main`.
    pub {{ f[4] }}: crate::link::{{ f[4] }}::Link,
{%- if f[3] == "protobuf" %}
    /// Буферы сообщений узла `HOST_{{ f[4] | upcase }}` (`bsp::link::{{ f[4] }}`).
    pub {{ f[4] }}_buffers: crate::link::HostBuffers,
{%- endif %}
{%- if f[0] == "usb" %}
    /// Устройство `embassy-usb` `{{ f[2] }}` для узла `USB_{{ f[4] | upcase }}` (`bsp::usb::{{ f[4] }}`). Драйвер —
    /// заглушка, пока проект не подставит USB-блок своей платы
    /// (`docs/modules/bsp-usb.md`). Поток CDC уже под транспортом протокола (`{{ f[4] }}`).
    pub {{ f[4] }}_device: crate::usb::{{ f[4] }}::Device,
{%- endif %}
{%- elsif f[0] == "uart" %}
    /// UART `{{ f[2] }}` — заглушка `bsp::serial::{{ f[4] }}::Serial`, пока проект не подставит
    /// драйвер своей платы (`docs/modules/bsp-serial.md`). Протокола у транспорта
    /// нет: поле ждёт ваш узел.
    pub {{ f[4] }}: crate::serial::{{ f[4] }}::Serial,
{%- elsif f[0] == "can" %}
    /// Шина CAN `{{ f[2] }}`{% if f[3] == "canopen" %} под узлом `CANOPEN_{{ f[4] | upcase }}`{% endif %} — заглушка `bsp::can::{{ f[4] }}::Can`, пока проект не
    /// подставит контроллер своей платы (`docs/modules/bsp-can.md`).
{%- if f[3] == "canopen" %}
    /// Уезжает в узел `CANOPEN_{{ f[4] | upcase }}`: в граф (`fragments:` в `crates-cross/app/src/graph.rs`) или,
    /// при `graph=no`, в задачу, которую спавнит `main`.
{%- else %}
    /// Протокола у транспорта нет: поле ждёт ваш узел.
{%- endif %}
    pub {{ f[4] }}: crate::can::{{ f[4] }}::Can,
{%- elsif f[0] == "usb" %}
    /// USB `{{ f[2] }}`: устройство для узла `USB_{{ f[4] | upcase }}` и поток CDC (`bsp::usb::{{ f[4] }}`).
    /// Драйвер — заглушка, пока проект не подставит USB-блок своей платы
    /// (`docs/modules/bsp-usb.md`). Протокола у транспорта нет: поток ждёт ваш узел.
    pub {{ f[4] }}: crate::usb::{{ f[4] }}::Usb,
{%- endif %}
{%- endfor %}
{%- if has_net == "true" %}
    /// Сеть: фоновая работа стека для узла `NET` и сам стек{% if protocol_sockets == "0" %}, на котором
    /// приложение открывает сокет{% endif %} (`bsp::net`). Устройство — заглушка, пока проект не подставит
    /// Ethernet своей платы (`docs/modules/bsp-net.md`){% if protocol_sockets != "0" %}. Сокеты протоколов на стеке уже
    /// открыты (`bsp::link`){% endif %}.
    pub net: crate::net::Net,
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

// Вручную: у части объектов платы (`UsbDevice` из `embassy-usb`) `Debug` нет, а
// печатать целую плату незачем — сведения о чипе лежат в `spec`.
impl core::fmt::Debug for Board {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Board")
            .field("spec", &self.spec)
            .finish_non_exhaustive()
    }
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
{%- assign usb_links = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" and f[3] != "none" %}{% assign usb_links = usb_links | plus: 1 %}{% endif %}
{%- endfor %}
{%- if usb_links > 1 %}
    #[expect(
        clippy::similar_names,
        reason = "имена транспортов — имена блоков USB, они различаются буквой (`usb_otg_fs`, `usb_otg_hs`)"
    )]
{%- endif %}
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
        let p = init_peripherals(config());
{%- if cache != "" %}
        // Сразу за HAL (латентность флеша и тактирование выставлены) и до всего
        // остального: кэши с завода выключены (см. `crate::cache`).
        crate::cache::enable();
{%- endif %}
        // Тоже сразу за HAL: ловушка деления, и обработчик `HardFault` попадает в образ.
        crate::fault::enable();
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
{%- if has_net == "true" %}
{%- if rng_irq != "" %}
        let seed = crate::rng::seed(r.rng.rng);
{%- else %}
        let seed = crate::net::seed_from_uid(embassy_stm32::uid::uid());
{%- endif %}
        let net = crate::net::new(embassy_stm32::uid::uid(), seed, r.eth);
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" and f[3] != "none" %}
        let adapters::usb::Usb {
            device: {{ f[4] }}_device,
            cdc: {{ f[4] }}_cdc,
        } = crate::usb::{{ f[4] }}::new(r.{{ f[4] }});
{%- endif %}
{%- endfor %}

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
{%- if core_baseline != "true" %}
            cycles: crate::cycles::Cycles::new(sys_hz),
{%- endif %}
{%- if ota == "true" %}
            ota: crate::ota::new({% if ota_store == "external" %}external{% elsif dfu_bank == "true" %}flash, dfu_bank{% else %}flash{% endif %}),
{%- endif %}
{%- if config == "true" %}
            settings: crate::settings::new({% if config_store != "external" %}flash{% elsif ota == "true" and ota_store == "external" %}external{% endif %}),
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "modbus" or f[3] == "protobuf" %}
{%- if f[0] == "uart" %}
            {{ f[4] }}: crate::link::{{ f[4] }}::new(crate::serial::{{ f[4] }}::new(r.{{ f[4] }})),
{%- elsif f[0] == "usb" %}
            {{ f[4] }}_device,
            {{ f[4] }}: crate::link::{{ f[4] }}::new({{ f[4] }}_cdc),
{%- else %}
            {{ f[4] }}: crate::link::{{ f[4] }}::new(net.stack),
{%- endif %}
{%- if f[3] == "protobuf" %}
            {{ f[4] }}_buffers: crate::link::{{ f[4] }}::host_buffers(),
{%- endif %}
{%- elsif f[0] == "uart" %}
            {{ f[4] }}: crate::serial::{{ f[4] }}::new(r.{{ f[4] }}),
{%- elsif f[0] == "can" %}
            {{ f[4] }}: crate::can::{{ f[4] }}::new(r.{{ f[4] }}),
{%- elsif f[0] == "usb" %}
            {{ f[4] }}: crate::usb::{{ f[4] }}::new(r.{{ f[4] }}),
{%- endif %}
{%- endfor %}
{%- if has_net == "true" %}
            // Последним из сетевых: транспорты выше берут `net.stack` (`Copy`).
            net,
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
/// Поднимает HAL — то, с чего начинают `Board::new` и bootloader.
///
/// Отдельной функцией ради `boot`: тот поднимает HAL на
/// `embassy_stm32::Config::default()`, а не на конфиге платы, и `Board` ему не
/// нужен. Приложению — только через [`Board::new`].
#[must_use]
pub fn init_peripherals(config: embassy_stm32::Config) -> embassy_stm32::Peripherals {
    static SHARED_DATA: core::mem::MaybeUninit<embassy_stm32::SharedData> =
        core::mem::MaybeUninit::uninit();
    embassy_stm32::init_primary(config, &SHARED_DATA)
}
{%- else %}

/// Поднимает HAL — то, с чего начинают `Board::new` и bootloader.
///
/// Отдельной функцией ради `boot`: тот поднимает HAL на
/// `embassy_stm32::Config::default()`, а не на конфиге платы, и `Board` ему не
/// нужен. Приложению — только через [`Board::new`].
#[must_use]
pub fn init_peripherals(config: embassy_stm32::Config) -> embassy_stm32::Peripherals {
    embassy_stm32::init(config)
}
{%- endif %}

/// Конфиг HAL: умолчания `embassy-stm32`{% if has_net == "true" %}{% if rng_clock != "" and rng_clock != "default" %} и клок блока `RNG` для сетевого
/// стека, которого они не дают (`docs/modules/bsp-rng.md`){% endif %}{% endif %}.
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
