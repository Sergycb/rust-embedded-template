#![no_std]

pub mod buffers;
{%- if config == "true" %}
pub mod config;
{%- endif %}
{%- if ota == "true" %}
pub mod ota;
{%- endif %}
pub mod resources;

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
        // сохраняется. Когда появится распиновка платы, эти поля разложит
        // `assign_resources!` (см. resources.rs), и `Board` начнёт отдавать их
        // задачам — сейчас отдавать нечего.
        let flash = FLASH.init(Mutex::new(RefCell::new(Flash::new_blocking(p.FLASH))));

        Self {
            clocks,
{%- if ota == "true" %}
            ota: ota::Ota::new(flash),
{%- endif %}
{%- if config == "true" %}
            settings: config::Settings::new(flash),
{%- endif %}
        }
{%- else %}
        Self { clocks, p }
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
