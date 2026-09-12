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
pub type FlashMutex = Mutex<NoopRawMutex, RefCell<Flash<'static, Blocking>>>;

/// `&'static` он не для красоты: `Ota` и `Settings` отдаются в задачи, а
/// аргументы задач embassy обязаны быть `'static`. Обычное поле `Board` этого
/// не даёт — `Board` живёт в `main`, а не вечно.
static FLASH: StaticCell<FlashMutex> = StaticCell::new();
{%- endif %}

/// Объекты этой платы — всё, что приложение о ней знает.
///
/// Каждое поле реализует порт из `ports` (кроме сторожа, который реализует
/// `watchdog::HardwareWatchdog` — тот же приём границы, только трейт
/// приходит из крейта сторожа). Периферии в полях нет вовсе: разбирать её на
/// объекты — работа `bsp`, а `Peripherals`, пины и группы `assign_resources!`
/// остаются внутри [`Board::new`].
// Закрывающая скобка ниже — тоже по условию: без OTA, настроек и графа
// `Board` пуст, и rustfmt требует `{}` в одну строку, а не на двух — иначе
// `cargo fmt --check` в сгенерированном проекте падает.
pub struct Board {
{%- if ota == "true" %}
    /// Обновление прошивки: адаптер поверх разделов `DFU`/`BOOTLOADER_STATE`
    /// (`adapters::ota`), собранный из символов `memory.x`. Канал доставки —
    /// за пределами шаблона, см. `docs/modules/bsp-ota.md`.
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
{%- if ota == "true" or config == "true" or graph == "true" %}
}
{%- else %}}
{%- endif %}

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
    #[allow(clippy::new_without_default)]
    pub fn new() -> Self {
        let p = init_peripherals();
        // Единственный способ узнать фактическое тактирование: `Clocks`
        // заполняется внутри `init_peripherals`, наружу отдаётся только так.
        // Дамп в лог — не отладочный мусор, а штатная сводка старта: по нему
        // видно, какой источник выиграл (HSE не завёлся — HAL молча уедет на
        // HSI) и что получили делители. Полем `Board` частоты не остаются:
        // домену они не нужны, а тому, кто их спросит (target-тест
        // `clocks_match_intent`), доступен тот же `rcc::clocks`.
        //
        // Частота конкретного блока, когда `Board` начнёт отдавать периферию:
        // `defmt::info!("uart clk {}", rcc::frequency::<peripherals::USART1>().0)`
        // — она берётся не с шины, а через мультиплексор (`Config.rcc.mux`),
        // и от общего дампа может отличаться.
        info!(
            "bsp: board initialized, clocks {}",
            embassy_stm32::rcc::clocks(&p.RCC)
        );

{%- if ota == "true" or config == "true" %}
        // Периферия разбирается здесь: `FLASH` уходит в общий объект (его
        // делят OTA и настройки), остальное пока никому не нужно и потому не
        // сохраняется. Здесь же — место для распиновки платы: объявите
        // `assign_resources!` (docs/resources.md), добавьте `let r =
        // split_resources!(p);` и собирайте из `r` свои драйверы. Наружу из
        // `bsp` уходит не периферия, а готовый объект — поле `Board`,
        // реализующее порт из `ports`.
        let flash = FLASH.init(Mutex::new(RefCell::new(Flash::new_blocking(p.FLASH))));
{%- else %}
        // Место для распиновки платы: объявите `assign_resources!`
        // (docs/resources.md), добавьте `let r = split_resources!(p);` и
        // собирайте из `r` свои драйверы. Наружу из `bsp` уходит не
        // периферия, а готовый объект — поле `Board`, реализующее порт из
        // `ports`.
{%- endif %}

        Self {
{%- if ota == "true" %}
            ota: crate::ota::new(flash),
{%- endif %}
{%- if config == "true" %}
            settings: crate::config::new(flash),
{%- endif %}
{%- if graph == "true" %}
            watchdog: crate::wdg::UnarmedBoardWatchdog::new(p.{{watchdog_peripheral}}),
{%- endif %}
{%- if ota == "true" or config == "true" or graph == "true" %}
        }
{%- else %}}
{%- endif %}
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
