#![no_std]

pub mod buffers;
{%- if config == "true" %}
pub mod config;
{%- endif %}
{%- if ota == "true" %}
pub mod ota;
{%- endif %}
pub mod persist;
pub mod resources;
pub mod stack;
pub mod wdg;

use defmt::info;
{%- if ota == "true" or config == "true" %}

use core::cell::RefCell;

use embassy_stm32::flash::{Blocking, Flash};
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::NoopRawMutex;
use static_cell::StaticCell;

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

pub struct Board {
    /// Периферия самого ядра Cortex-M — та, что не зависит ни от чипа, ни от
    /// платы: `DWT`, `MPU`, `NVIC`, `SCB`, `SYST`, `DCB`, `CPUID`, `FPU`.
    ///
    /// Здесь, а не отдельным `cortex_m::Peripherals::take()` в вашем коде,
    /// потому что забрать её можно ровно один раз за запуск. Пока `Board`
    /// оставалась единственным местом, где начинается владение периферией
    /// ЧИПА, ядро выпадало из этой истории — и второй `take()` где-нибудь в
    /// задаче молча возвращал бы `None` уже в рантайме.
    ///
    /// Отдаётся целиком, без отбора: в проекте, собранном этим шаблоном,
    /// состав полей у `cortex_m::Peripherals` одинаков на всех ядрах —
    /// единственные условные поля (`AC` у Cortex-M7, `SCBNS`) закрыты
    /// Cargo-фичами `cortex-m`, которых шаблон не включает. А какой блок
    /// понадобится проекту — решать не шаблону.
    ///
    /// Что из этого обычно и берут:
    ///
    /// * `DWT` вместе с `DCB` — счётчик тактов для профилирования
    ///   (`DCB::enable_trace()`, затем `DWT::enable_cycle_counter()`); на
    ///   Armv6-M (Cortex-M0/M0+) блока нет, методы не соберутся;
    /// * `MPU` — защита стека от переползания в статические данные;
    /// * `SYST` — свободный таймер: драйвер времени embassy сидит на `TIM`,
    ///   а не на SysTick.
    ///
    /// Что лучше не трогать: `NVIC` — приоритеты прерываний держат драйверы
    /// embassy, а `VTOR` в `SCB` выставляет bootloader при прыжке в
    /// приложение.
    pub core: cortex_m::Peripherals,
    /// Частоты, которые HAL реально насчитал при инициализации — не те, что
    /// заказаны в `embassy_stm32::Config`, а получившиеся.
    ///
    /// Хранится копией (`Clocks` — `Copy`), потому что дескриптор `RCC` уходит
    /// внутрь `init_peripherals`, а `rcc::clocks()` требует ссылку на него.
    ///
    /// Зачем это вообще нужно: расхождение «заказали 168 МГц, получили 64»
    /// компилятор не ловит в принципе, а embassy проверяет только верхние
    /// границы шин (`assert!(hclk <= hclk_max)` и подобные) — конфигурация,
    /// дающая USB 47.5 МГц вместо 48 или UART с ошибкой по битрейту, проходит
    /// молча и всплывает потом как «железо иногда сыпет мусором». Поле
    /// печатается при старте (см. `init`) и стережётся target-тестом
    /// `clocks_match_intent`.
    pub clocks: embassy_stm32::rcc::Clocks,
    /// Счётчик запусков, переживающий программный сброс (но не пропадание
    /// питания). Приложение видит его через [`ports::BootCounter`] и про
    /// регион `PERSIST` не знает — в этом и смысл: чтение сырой памяти по
    /// адресу из линкерного скрипта живёт здесь, а наружу уходит объект.
    pub boot: persist::BootCount,
{%- if ota == "true" %}
    /// Обновление прошивки: запись образа в раздел `DFU` и пометки, по
    /// которым bootloader меняет разделы местами. Канал доставки — за
    /// пределами шаблона, см. модуль `ota`.
    pub ota: ota::Ota,
{%- endif %}
{%- if config == "true" %}
    /// Настройки, переживающие перезапуск и обновление прошивки: раздел
    /// `CONFIG` во flash. Формат значений выбирает проект, см. модуль
    /// `config`.
    pub settings: config::Settings,
{%- endif %}
{%- if ota != "true" and config != "true" %}
    // Not yet split into individual peripherals; kept whole until board wiring is added.
    #[allow(dead_code)]
    p: embassy_stm32::Peripherals,
{%- endif %}
}

impl Board {
    pub fn init() -> Self {
        // Узор по свободному стеку — до всего остального: чем раньше, тем
        // больше расхода попадёт в замер (см. `stack::never_used`). Раньше
        // этого места нельзя: заливка идёт по тому же стеку, на котором
        // работает.
        stack::paint();

        // Периферия ядра забирается ПЕРВОЙ, до инициализации HAL, и порядок
        // здесь существенный: `steal()` взводит тот же флаг «уже забрано»,
        // что и `take()`, а embassy-stm32 внутри своей инициализации зовёт
        // его на части семейств (STM32N6, см. `rcc/n6.rs`). Сделай мы
        // наоборот — на этих чипах `take()` возвращал бы `None`.
        let core = cortex_m::Peripherals::take().expect(
            "периферия ядра уже кем-то забрана: `Board::init()` должен быть первым, кто её \
             запрашивает",
        );
        let p = init_peripherals();
        // Единственный способ узнать фактическое тактирование: `Clocks`
        // заполняется внутри `init_peripherals`, наружу отдаётся только так.
        // Дамп в лог — не отладочный мусор, а штатная сводка старта: по нему
        // видно, какой источник выиграл (HSE не завёлся — HAL молча уедет на
        // HSI) и что получили делители.
        //
        // Частота конкретного блока, когда `Board` начнёт отдавать периферию:
        // `defmt::info!("uart clk {}", rcc::frequency::<peripherals::USART1>().0)`
        // — она берётся не с шины, а через мультиплексор (`Config.rcc.mux`),
        // и от `clocks` ниже может отличаться.
        let clocks = *embassy_stm32::rcc::clocks(&p.RCC);
        info!("bsp: board initialized, clocks {}", clocks);

{%- if ota == "true" or config == "true" %}
        // Периферия разбирается здесь: `FLASH` уходит в общий объект (его
        // делят OTA и настройки), остальное пока никому не нужно и потому не
        // сохраняется. Здесь же — место для распиновки платы: раскомментируйте
        // `assign_resources!` в resources.rs, добавьте `let r =
        // split_resources!(p);` и собирайте из `r` свои драйверы. Наружу из
        // `bsp` уходит не периферия, а готовый объект — поле `Board`,
        // реализующее порт из `ports`.
        let flash = FLASH.init(Mutex::new(RefCell::new(Flash::new_blocking(p.FLASH))));

        Self {
            core,
            clocks,
            boot: persist::BootCount::new(),
{%- if ota == "true" %}
            ota: ota::Ota::new(flash),
{%- endif %}
{%- if config == "true" %}
            settings: config::Settings::new(flash),
{%- endif %}
        }
{%- else %}
        Self {
            core,
            clocks,
            boot: persist::BootCount::new(),
            p,
        }
{%- endif %}
    }

    /// Причина, по которой упал предыдущий запуск, если он упал.
    ///
    /// Ассоциированная функция, а не метод: звать её нужно ДО [`Board::init`],
    /// и это не стилистика. Инициализация HAL сама умеет паниковать — не та
    /// конфигурация тактирования, занятая периферия ядра, негодный раздел
    /// `CONFIG`, — а на release-профиле паника это сброс. Будь причина
    /// доступна только через готовый `Board`, устройство, падающее внутри
    /// `init`, крутилось бы в цикле перезагрузок, ни разу не напечатав, из-за
    /// чего упало прошлое.
    ///
    /// Порта под это нет намеренно: домену причина прошлой паники не нужна,
    /// это диагностика старта (правило — в doc-комментарии крейта `ports`).
    ///
    /// Читается ровно один раз: `panic-persist` стирает свою магию, чтобы одно
    /// падение не всплывало после каждого сброса. Сохраняет причину
    /// `#[panic_handler]` в `app` — он не может уехать сюда, потому что это
    /// lang item, привязанный к бинарнику, а не к библиотеке.
    pub fn last_panic() -> Option<&'static str> {
        panic_persist::get_panic_message_utf8()
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
// CLAUDE.md/README, раздел про dual-core. Полноценная AMP-поддержка —
// осознанно не входит в шаблон.
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
