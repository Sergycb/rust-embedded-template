#![no_std]

pub mod buffers;
{%- if ota == "true" %}
pub mod ota;
{%- endif %}
pub mod resources;

use defmt::info;

pub struct Board {
{%- if ota == "true" %}
    /// Обновление прошивки: запись образа в раздел `DFU` и пометки, по
    /// которым bootloader меняет разделы местами. Канал доставки — за
    /// пределами шаблона, см. модуль `ota`.
    pub ota: ota::Ota,
{%- else %}
    // Not yet split into individual peripherals; kept whole until board wiring is added.
    #[allow(dead_code)]
    p: embassy_stm32::Peripherals,
{%- endif %}
}

impl Board {
    pub fn init() -> Self {
        let p = init_peripherals();
        info!("bsp: board initialized");

{%- if ota == "true" %}
        // Периферия разбирается здесь: `FLASH` забирает `Ota`, остальное пока
        // никому не нужно и потому не сохраняется. Когда появится распиновка
        // платы, эти поля разложит `assign_resources!` (см. resources.rs), и
        // `Board` начнёт отдавать их задачам — сейчас отдавать нечего.
        Self {
            ota: ota::Ota::new(p.FLASH),
        }
{%- else %}
        Self { p }
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
