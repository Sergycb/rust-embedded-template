//! Объекты платы: `Board` собирает их из периферии чипа и отдаёт приложению.
//!
//! Границу `bsp` → `app` пересекают только они — объекты, реализующие порты из
//! `ports`. Ни `Peripherals`, ни отдельные пины, ни периферия ядра наружу не
//! выходят: что не разобрано на объекты, то и не отдано (docs/architecture.md,
//! «`Board` отдаёт объекты, а не периферию»).

{%- if shared_flash == "true" %}

use core::cell::RefCell;
{%- endif %}

use defmt::info;
{%- if shared_flash == "true" %}
{%- if dfu_bank == "true" %}
use embassy_stm32::flash::{Bank1Region, Bank2Region};
{%- endif %}
use embassy_stm32::flash::{Blocking, Flash};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use static_cell::StaticCell;
{%- endif %}

{%- if shared_flash == "true" %}

/// Общий `Flash` всего чипа. Контроллер флеша один, а работать с ним нужно и
/// OTA (разделы `DFU`/`BOOTLOADER_STATE`), и настройкам (раздел `CONFIG` во
/// флеше или внутренняя EEPROM — она пишется через тот же контроллер), —
/// поэтому объект создаётся ровно один и раздаётся ссылками.
///
/// `NoopRawMutex`, а не `CriticalSectionRawMutex`: обе половины работают из
/// задач одного исполнителя, то есть без вытеснения. Появится второй
/// исполнитель (прерывательный приоритет, второе ядро) — менять здесь.
///
/// `RefCell` внутри нужен блокирующему `Mutex` embassy, поэтому ячейка
/// спрятана в newtype: наружу тип торчит лишь как параметр
/// [`adapters::flash::Shared`].
pub struct FlashMutex(Mutex<NoopRawMutex, RefCell<ChipFlash>>);

{%- if dfu_bank == "true" %}

/// Флеш под [`FlashMutex`] — банк 1, а не весь чип: банк 2 стоит за
/// разрывом, и цельный `Flash` туда не дотягивается (`FLASH_SIZE` у него —
/// сумма банков, меньше смещения банка 2). Смещения в банке 1 те же, что у
/// цельного `Flash`, — от базы flash.
pub type ChipFlash = Bank1Region<'static, Blocking>;

/// Банк 2 — под `DFU`, и только под него: смещения в нём отсчитаны от его
/// начала, и символы `memory.x` у `DFU` посчитаны так же. Без newtype, в
/// отличие от [`FlashMutex`]: наружу тип не выходит, он нужен одному
/// `bsp::ota`, а `FlashLock` для голого мьютекса уже есть в `adapters`.
pub type DfuBankMutex = Mutex<NoopRawMutex, RefCell<Bank2Region<'static, Blocking>>>;

/// `&'static` — по той же причине, что [`FLASH`].
static DFU_BANK: StaticCell<DfuBankMutex> = StaticCell::new();
{%- else %}

/// Флеш под [`FlashMutex`] — весь чип одним объектом.
pub type ChipFlash = Flash<'static, Blocking>;
{%- endif %}

impl FlashMutex {
    fn new(flash: ChipFlash) -> Self {
        Self(Mutex::new(RefCell::new(flash)))
    }
}

impl adapters::flash::FlashLock for FlashMutex {
    type Flash = ChipFlash;

    fn with<R>(&self, op: impl FnOnce(&mut Self::Flash) -> R) -> R {
        self.0.with(op)
    }
}

impl core::fmt::Debug for FlashMutex {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("FlashMutex").finish_non_exhaustive()
    }
}

/// `&'static` он не для красоты: `Ota` и `Settings` отдаются в задачи, а
/// аргументы задач embassy обязаны быть `'static`. Обычное поле `Board` этого
/// не даёт — `Board` живёт в `main`, а не вечно.
static FLASH: StaticCell<FlashMutex> = StaticCell::new();
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
    /// Обновление прошивки — вход узла OTA: канал доставки `link` (заглушка
    /// `bsp::ota::Link`, пока проект не подставит свой транспорт) и адаптер
    /// `flash` поверх разделов `DFU`/`BOOTLOADER_STATE`, собранный из символов
    /// `memory.x`. {% if graph == "true" %}Уезжает в узел `OTA` графа (`fragments:` в
    /// `crates-cross/app/src/graph.rs`){% else %}Зовите узел сами из `main`:
    /// `domain::ota::run` с контекстом `domain::ota::OtaCtx`, собранным руками
    /// из `link`, `flash` и `domain::ota::Mode::{% if signed == "true" %}Signed` с версией
    /// образа (`crates-cross/app/src/image.rs`){% else %}Plain`{% endif %}{% endif %}.
    pub ota: crate::ota::Ota,
{%- endif %}
{%- if config == "true" %}
    /// Настройки, переживающие перезапуск и обновление прошивки:
    /// `adapters::settings` поверх {% if config_store == "eeprom" %}внутренней EEPROM{% elsif config_store == "external" %}внешней флеши (драйвер — `bsp/src/{% if ota == "true" and ota_store == "external" %}external{% else %}config{% endif %}.rs`){% else %}раздела `CONFIG` во флеше{% endif %}.
    /// Формат значений выбирает проект.
    pub settings: crate::config::Settings,
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

{%- if shared_flash == "true" %}
        // Периферия разбирается здесь: `FLASH` уходит в общий объект (его
        // делят OTA и настройки), остальное пока никому не нужно и потому не
        // сохраняется. Здесь же — место для распиновки платы: объявите
        // `assign_resources!` (docs/resources.md), добавьте `let r =
        // split_resources!(p);` и собирайте из `r` свои драйверы. Наружу из
        // `bsp` уходит не периферия, а готовый объект — поле `Board`,
        // реализующее порт из `ports`.
{%- if dfu_bank == "true" %}
        let banks = Flash::new_blocking(p.FLASH).into_blocking_regions();
        let flash = FLASH.init(FlashMutex::new(banks.bank1_region));
        let dfu_bank = DFU_BANK.init(Mutex::new(RefCell::new(banks.bank2_region)));
{%- else %}
        let flash = FLASH.init(FlashMutex::new(Flash::new_blocking(p.FLASH)));
{%- endif %}
{%- else %}
        // Место для распиновки платы: объявите `assign_resources!`
        // (docs/resources.md), добавьте `let r = split_resources!(p);` и
        // собирайте из `r` свои драйверы. Наружу из `bsp` уходит не
        // периферия, а готовый объект — поле `Board`, реализующее порт из
        // `ports`.
{%- endif %}
{%- if ota == "true" and ota_store == "external" %}
        // Внешняя флеш — одна на `STATE`/`DFU`{% if config_store == "external" %} и настройки{% endif %}: драйвер
        // собирается здесь же, из периферии (см. `crate::external`).
        let external = crate::external::new();
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
            ota: crate::ota::new({% if ota_store == "external" %}external{% elsif dfu_bank == "true" %}flash, dfu_bank{% else %}flash{% endif %}),
{%- endif %}
{%- if config == "true" %}
            settings: crate::config::new({% if config_store != "external" %}flash{% elsif ota == "true" and ota_store == "external" %}external{% endif %}),
{%- endif %}
{%- if interface == "uart" %}
            serial: crate::serial::new(),
{%- endif %}
{%- if interface == "can" %}
            can: crate::can::new(),
{%- endif %}
{%- if graph == "true" %}
            watchdog: crate::wdg::UnarmedBoardWatchdog::new(p.{{watchdog_peripheral}}),
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
    embassy_stm32::init_primary(embassy_stm32::Config::default(), &SHARED_DATA)
}
{%- else %}

fn init_peripherals() -> embassy_stm32::Peripherals {
    embassy_stm32::init(embassy_stm32::Config::default())
}
{%- endif %}
