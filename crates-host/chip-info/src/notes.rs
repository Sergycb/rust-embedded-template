//! Пометки к выводам, занятым не периферией: отладочный порт, кварц, MCO.

use stm32_metapac::metadata::METADATA;

/// Выводы, которые заняты не периферией, а отладкой, и потому в метаданных не
/// помечены никак: `stm32-data` описывает только периферийные сигналы, а
/// отладочный порт к периферии не относится. Значения одинаковы для всех
/// STM32 (это фиксированная привязка ядра Cortex-M у ST), поэтому таблица
/// зашита, а не вычисляется.
///
/// Разделение на «занят» и «свободен в SWD» существенно: `PA13`/`PA14` держит
/// сам отладчик — заняв их, вы теряете плату до `probe-rs erase
/// --connect-under-reset`, — а `PB3`/`PA15`/`PB4` нужны только полному JTAG и
/// в режиме SWD доступны как обычные GPIO.
/// Оно же — разделение на «ошибка» и «предупреждение» для `--check`.
const DEBUG_PINS: &[(&str, Severity, &str)] = &[
    (
        "PA13",
        Severity::Conflict,
        "SWDIO — отладочный порт, занимать нельзя",
    ),
    (
        "PA14",
        Severity::Conflict,
        "SWCLK — отладочный порт, занимать нельзя",
    ),
    (
        "PB3",
        Severity::Warning,
        "JTDO/SWO — свободен в режиме SWD (но это же вывод трассировки SWO)",
    ),
    ("PA15", Severity::Warning, "JTDI — свободен в режиме SWD"),
    ("PB4", Severity::Warning, "NJTRST — свободен в режиме SWD"),
];

/// Насколько плохо занять вывод под своё.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Severity {
    /// Плата теряет отладку — `--check` завершается ненулевым кодом.
    Conflict,
    /// Зависит от того, как разведена и настроена плата (кварц может быть не
    /// нужен, JTAG в режиме SWD свободен): `--check` печатает и не падает.
    Warning,
}

/// Предупреждение для вывода, который занят чем-то, кроме периферии.
///
/// Отладочный порт — из зашитой таблицы (в метаданных его нет), выводы кварца
/// и MCO — из самих метаданных: они описаны как сигналы блока `RCC`, и
/// выдумывать для них список не нужно.
pub(crate) fn pin_note(pin: &str) -> Option<(Severity, String)> {
    if let Some((_, severity, note)) = DEBUG_PINS.iter().find(|(name, _, _)| *name == pin) {
        return Some((*severity, (*note).to_owned()));
    }
    let rcc = METADATA
        .peripherals
        .iter()
        .find(|peripheral| peripheral.name == "RCC")?;
    let signal = rcc.pins.iter().find(|rcc_pin| rcc_pin.pin == pin)?.signal;
    let note = match signal {
        signal if signal.starts_with("OSC32") => {
            format!("{signal} — вывод часового кварца 32 кГц (занят, если LSE используется)")
        }
        signal if signal.starts_with("OSC") => {
            format!("{signal} — вывод кварца (занят, если тактирование от HSE)")
        }
        signal => format!("{signal} — вывод тактового выхода RCC"),
    };
    // Всегда предупреждение, а не конфликт: кварц может быть не разведён, а
    // MCO — не использоваться, и тогда занимать эти выводы совершенно законно.
    Some((Severity::Warning, note))
}
