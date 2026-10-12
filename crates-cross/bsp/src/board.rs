//! Объекты платы: `Board` собирает их из периферии чипа и отдаёт приложению.
//!
//! Границу `bsp` → `app` пересекают только они — объекты, реализующие порты из
//! `ports`. Ни `Peripherals`, ни отдельные пины, ни периферия ядра наружу не
//! выходят: что не разобрано на объекты, то и не отдано (docs/architecture.md,
//! «`Board` отдаёт объекты, а не периферию»).

{%- if ota == "true" or config == "true" %}

use core::cell::RefCell;
{%- endif %}

use defmt::info;
{%- if ota == "true" or config == "true" %}
use embassy_stm32::flash::{Blocking, Flash};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use static_cell::StaticCell;
{%- endif %}

{%- if ota == "true" or config == "true" %}

/// Общий `Flash` всего чипа. Контроллер флеша один, а работать с ним нужно и
/// OTA (разделы `DFU`/`BOOTLOADER_STATE`), и настройкам (раздел `CONFIG`), —
/// поэтому объект создаётся ровно один и раздаётся ссылками.
///
/// `NoopRawMutex`, а не `CriticalSectionRawMutex`: обе половины работают из
/// задач одного исполнителя, то есть без вытеснения. Появится второй
/// исполнитель (прерывательный приоритет, второе ядро) — менять здесь.
///
/// `RefCell` внутри нужен блокирующему `Mutex` embassy (`embassy-boot` и
/// `BlockingPartition` принимают только его), поэтому ячейка спрятана в
/// newtype: наружу тип торчит лишь как параметр [`adapters::flash::Shared`].
pub struct FlashMutex(Mutex<NoopRawMutex, RefCell<Flash<'static, Blocking>>>);

impl FlashMutex {
    fn new(flash: Flash<'static, Blocking>) -> Self {
        Self(Mutex::new(RefCell::new(flash)))
    }

{%- if ota == "true" %}

    /// Мьютекс в том виде, в каком его ждёт `embassy-boot`.
    pub(crate) const fn raw(&self) -> &Mutex<NoopRawMutex, RefCell<Flash<'static, Blocking>>> {
        &self.0
    }
{%- endif %}
}

impl adapters::flash::FlashLock for FlashMutex {
    type Flash = Flash<'static, Blocking>;

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

/// Объекты этой платы — всё, что приложение о ней знает.
///
/// Каждое поле — адаптер, реализующий порт из `ports` (кроме сторожа, который
/// реализует `watchdog::HardwareWatchdog` — тот же приём границы, только трейт
/// приходит из крейта сторожа), или данные, тип которых объявлен там же
/// (`spec`). Периферии в полях нет вовсе: разбирать её на объекты — работа
/// `bsp`, а `Peripherals`, пины и группы `assign_resources!` остаются внутри
/// [`Board::new`].
#[derive(Debug)]
pub struct Board {
    /// Сведения о чипе: имя, заводской номер, объём flash, частоты `sys` и `hclk1`.
    pub spec: ports::ChipSpec,
{%- if ota == "true" %}
    /// Обновление прошивки — вход узла OTA: канал доставки `link` (заглушка
    /// `bsp::ota::Link`, пока проект не подставит свой транспорт) и адаптер
    /// `flash` поверх разделов `DFU`/`BOOTLOADER_STATE`, собранный из символов
    /// `memory.x`. {% if graph == "true" %}Уезжает в узел `OTA` графа (`fragments:` в
    /// `crates-cross/app/src/graph.rs`){% else %}Зовите узел сами из `main`:
    /// `domain::ota::run` с контекстом `domain::ota::OtaCtx`, собранным руками
    /// из `link`, `flash` и `domain::ota::Mode::{% if signed == "true" %}signed{% else %}plain{% endif %}()`{% if signed == "true" %},
    /// обернув `flash` подписью (`crates-cross/app/src/image.rs`){% endif %}{% endif %}.
    pub ota: crate::ota::Ota,
{%- endif %}
{%- if config == "true" %}
    /// Настройки, переживающие перезапуск и обновление прошивки: адаптер
    /// поверх раздела `CONFIG` (`adapters::settings`). Формат значений
    /// выбирает проект.
    pub settings: crate::config::Settings,
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
        let p = init_peripherals();
        // Единственный способ узнать фактическое тактирование: `Clocks`
        // заполняется внутри `init_peripherals`, наружу отдаётся только так.
        // Дамп в лог — не отладочный мусор, а штатная сводка старта: по нему
        // видно, какой источник выиграл (HSE не завёлся — HAL молча уедет на
        // HSI) и что получили делители. В `ChipSpec` уходят только `sys` и
        // `hclk1` числами: целый `Clocks` — тип HAL, генерируется под чип.
        //
        // Частота конкретного блока, когда `Board` начнёт отдавать периферию:
        // `defmt::info!("uart clk {}", rcc::frequency::<peripherals::USART1>().0)`
        // — она берётся не с шины, а через мультиплексор (`Config.rcc.mux`),
        // и от общего дампа может отличаться.
        let clocks = embassy_stm32::rcc::clocks(&p.RCC);
        info!("bsp: board initialized, clocks {}", clocks);

{%- if ota == "true" or config == "true" %}
        // Периферия разбирается здесь: `FLASH` уходит в общий объект (его
        // делят OTA и настройки), остальное пока никому не нужно и потому не
        // сохраняется. Здесь же — место для распиновки платы: объявите
        // `assign_resources!` (docs/resources.md), добавьте `let r =
        // split_resources!(p);` и собирайте из `r` свои драйверы. Наружу из
        // `bsp` уходит не периферия, а готовый объект — поле `Board`,
        // реализующее порт из `ports`.
        let flash = FLASH.init(FlashMutex::new(Flash::new_blocking(p.FLASH)));
{%- else %}
        // Место для распиновки платы: объявите `assign_resources!`
        // (docs/resources.md), добавьте `let r = split_resources!(p);` и
        // собирайте из `r` свои драйверы. Наружу из `bsp` уходит не
        // периферия, а готовый объект — поле `Board`, реализующее порт из
        // `ports`.
{%- endif %}

        let sys_hz = clocks.sys.to_hertz().map(|hz| hz.0);
        let hclk1_hz = clocks.hclk1.to_hertz().map(|hz| hz.0);

        // `usize` на Cortex-M — 32 бита, усечения нет.
        #[expect(
            clippy::cast_possible_truncation,
            reason = "`usize` на Cortex-M — 32 бита"
        )]
        let flash_size = embassy_stm32::flash::FLASH_SIZE as u32;

        Self {
            spec: ports::ChipSpec {
                chip: "{{chip}}",
                uid: embassy_stm32::uid::uid(),
                flash_size,
                sys_hz,
                hclk1_hz,
            },
{%- if ota == "true" %}
            ota: crate::ota::new(flash),
{%- endif %}
{%- if config == "true" %}
            settings: crate::config::new(flash),
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
