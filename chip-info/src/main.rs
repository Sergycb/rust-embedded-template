//! Справочник по чипу проекта: какие пины несёт периферийный блок, с какими
//! альтернативными функциями, какие DMA-каналы к нему привязаны и от чего он
//! тактируется. Запускается как `cargo xtask pins [<БЛОК>|<ПИН>]`.
//!
//! **Чего здесь намеренно нет — генерации кода.** Распиновку проверяет
//! компилятор: `SpiSck<SPI1>` для чужого пина просто не реализован, `Peri` не
//! `Copy`, поэтому один и тот же пин нельзя отдать двум задачам, а чип-фича
//! `stm32-metapac` пакетная — пина, которого нет в корпусе, не существует и в
//! типах. Генератор `resources.rs` из какого-нибудь `board.toml` дублировал бы
//! эти проверки и добавил второй источник правды. Компилятор не знает ровно
//! трёх вещей, ради которых справочник и написан:
//!
//! 1. какие пины вообще умеют нужный сигнал (иначе — даташит или CubeMX);
//! 2. не заняли ли вы отладочный порт или выводы кварца — типы про это молчат;
//! 3. от какого клока питается блок (нужно, когда считаешь baudrate или
//!    ловишь «периферия работает не на той частоте»).
//!
//! Данные — `stm32_metapac::metadata::METADATA`, то есть ровно тот же
//! `stm32-data`, из которого `embassy-stm32` генерирует свои типы: расхождения
//! между справочником и тем, что примет компилятор, быть не может.

use std::{env, fs, path::PathBuf};

use anyhow::{Context, bail};
use stm32_metapac::metadata::{
    METADATA, Peripheral, PeripheralDmaChannel, PeripheralRccKernelClock,
};

/// Чип-фича, под которую собран этот справочник. Подставляется при генерации
/// в двух местах сразу — здесь и в `[dependencies]` манифеста, — потому что
/// `stm32-metapac` выбирает чип фичей, а знать выбранное значение нужно ещё и
/// в рантайме, для проверки на рассинхрон (см. [`warn_on_chip_drift`]).
const CHIP_FEATURE: &str = "{{chip_feature}}";

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
const DEBUG_PINS: &[(&str, &str)] = &[
    ("PA13", "SWDIO — отладочный порт, занимать нельзя"),
    ("PA14", "SWCLK — отладочный порт, занимать нельзя"),
    (
        "PB3",
        "JTDO/SWO — свободен в режиме SWD (но это же вывод трассировки SWO)",
    ),
    ("PA15", "JTDI — свободен в режиме SWD"),
    ("PB4", "NJTRST — свободен в режиме SWD"),
];

fn main() -> Result<(), anyhow::Error> {
    warn_on_chip_drift();

    let args = env::args().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [] => {
            list_blocks();
            Ok(())
        }
        [query] => describe(query),
        other => bail!(
            "ожидался один аргумент (блок или пин), получено {}: {}",
            other.len(),
            other.join(" "),
        ),
    }
}

/// Печатает, с чего начинать: блоки, у которых вообще есть выводы.
///
/// Без аргумента команда именно перечисляет блоки, а не вываливает всю
/// распиновку: имена вроде `SPI1`/`USART2`/`TIM3` угадываются, а вот есть ли
/// на конкретном чипе `SAI1` или `FDCAN2` — нет.
fn list_blocks() {
    print_header(None);

    let mut names = METADATA
        .peripherals
        .iter()
        .filter(|peripheral| !peripheral.pins.is_empty())
        .map(|peripheral| peripheral.name)
        .collect::<Vec<_>>();
    names.sort_unstable_by_key(|name| block_sort_key(name));

    println!("блоки с выводами ({}):", names.len());
    for line in wrap(&names, 72) {
        println!("  {line}");
    }
    println!();
    println!("подробнее:");
    println!("  cargo xtask pins SPI1   # выводы, DMA, тактирование и прерывания блока");
    println!("  cargo xtask pins PA9    # что умеет конкретный вывод");
}

/// Один аргумент — это либо блок (`SPI1`), либо вывод (`PA9`). Различаются они
/// по форме имени, а не по порядку поиска: `PA9` не может быть блоком, а
/// `SPI1` — выводом, так что ошибиться нечем.
fn describe(query: &str) -> Result<(), anyhow::Error> {
    let query = query.to_uppercase();
    if looks_like_pin(&query) {
        describe_pin(&query)
    } else {
        describe_peripheral(&query)
    }
}

/// `P` + буква порта + число: `PA9`, `PH1`. Суффиксы вроде `PA0_C`
/// (аналоговый вывод-двойник на H7) тоже проходят — они есть в метаданных под
/// такими именами.
fn looks_like_pin(query: &str) -> bool {
    let mut chars = query.chars();
    chars.next() == Some('P')
        && chars.next().is_some_and(|port| port.is_ascii_uppercase())
        && chars.next().is_some_and(|digit| digit.is_ascii_digit())
}

fn describe_peripheral(name: &str) -> Result<(), anyhow::Error> {
    let peripheral = METADATA
        .peripherals
        .iter()
        .find(|peripheral| peripheral.name == name)
        .with_context(|| {
            format!(
                "на {} нет блока {name} — список: cargo xtask pins",
                METADATA.name
            )
        })?;

    print_header(Some(name));

    if peripheral.pins.is_empty() {
        println!("выводов нет (блок не выходит наружу)");
    } else {
        println!("выводы:");
        let mut pins = peripheral.pins.iter().collect::<Vec<_>>();
        pins.sort_by_key(|pin| (pin_sort_key(pin.pin), pin.signal));
        let width = pins.iter().map(|pin| pin.signal.len()).max().unwrap_or(0);
        for pin in pins {
            let af = match pin.af {
                Some(af) => format!("AF{af}"),
                // Пины без AF — аналоговые входы и выводы кварца: у них нет
                // альтернативной функции, они включаются другим регистром.
                None => "—".to_owned(),
            };
            let note = match pin_note(pin.pin) {
                Some(note) => format!("   ! {note}"),
                None => String::new(),
            };
            // `trim_end`, потому что у большинства строк примечания нет, а
            // выравнивающие пробелы после последней колонки остались бы в
            // выводе и попали в любой diff, куда его вставят.
            let line = format!(
                "  {:<6} {:<width$}  {:<4}{note}",
                pin.pin,
                pin.signal,
                af,
                width = width,
            );
            println!("{}", line.trim_end());
        }
    }

    if !peripheral.dma_channels.is_empty() {
        println!();
        println!("DMA:");
        let width = peripheral
            .dma_channels
            .iter()
            .map(|channel| channel.signal.len())
            .max()
            .unwrap_or(0);
        for channel in peripheral.dma_channels {
            println!(
                "  {:<width$}  {}",
                channel.signal,
                format_dma(channel),
                width = width,
            );
        }
    }

    print_clocks(peripheral);

    if !peripheral.interrupts.is_empty() {
        println!();
        println!("прерывания:");
        let width = peripheral
            .interrupts
            .iter()
            .map(|interrupt| interrupt.signal.len())
            .max()
            .unwrap_or(0);
        for interrupt in peripheral.interrupts {
            println!(
                "  {:<width$}  {}",
                interrupt.signal,
                interrupt.interrupt,
                width = width,
            );
        }
    }

    Ok(())
}

fn describe_pin(name: &str) -> Result<(), anyhow::Error> {
    if !METADATA.pins.iter().any(|pin| pin.name == name) {
        bail!(
            "вывода {name} нет в корпусе {} — метаданные пакетные, значит его нет и в типах \
			 embassy",
            METADATA.name,
        );
    }

    print_header(Some(name));

    if let Some(note) = pin_note(name) {
        println!("! {note}");
        println!();
    }

    let mut functions = METADATA
        .peripherals
        .iter()
        .flat_map(|peripheral| {
            peripheral
                .pins
                .iter()
                .filter(|pin| pin.pin == name)
                .map(move |pin| (peripheral.name, pin.signal, pin.af))
        })
        .collect::<Vec<_>>();
    functions.sort_unstable_by_key(|(block, signal, _)| (block_sort_key(block), *signal));

    if functions.is_empty() {
        println!("периферийных функций нет — только GPIO");
        return Ok(());
    }

    println!("функции ({}):", functions.len());
    let width = functions
        .iter()
        .map(|(block, _, _)| block.len())
        .max()
        .unwrap_or(0);
    for (block, signal, af) in functions {
        match af {
            Some(af) => println!("  {block:<width$}  {signal:<12} AF{af}", width = width),
            None => println!("  {block:<width$}  {signal:<12} —", width = width),
        }
    }

    Ok(())
}

/// Шина и источник тактирования блока.
///
/// Печатается вместе с выводами не для красоты: `kernel clock` — это то, из
/// чего периферия считает свой baudrate/период, и на большинстве современных
/// семейств он берётся не с шины, а через отдельный мультиплексор
/// (`embassy_stm32::rcc::mux`). Когда UART «врёт по скорости», а частота ядра
/// заведомо верная, смотреть надо именно сюда.
fn print_clocks(peripheral: &Peripheral) {
    let Some(rcc) = &peripheral.rcc else {
        return;
    };
    println!();
    println!("тактирование:");
    println!("  шина     {}", rcc.bus_clock);
    match &rcc.kernel_clock {
        PeripheralRccKernelClock::Clock(clock) => println!("  источник {clock}"),
        PeripheralRccKernelClock::Mux(mux) => println!(
            "  источник выбирается мультиплексором {}.{} (embassy: Config.rcc.mux)",
            mux.register, mux.field,
        ),
    }
}

/// Всё, что метаданные знают о привязке канала: на одних семействах это
/// фиксированный канал/поток, на других — номер запроса через DMAMUX, на
/// третьих есть ещё и ремап. Поэтому не разбор по семействам, а перечисление
/// того, что заполнено.
fn format_dma(channel: &PeripheralDmaChannel) -> String {
    let mut parts = Vec::new();
    if let Some(dma) = channel.dma {
        parts.push(format!("контроллер {dma}"));
    }
    if let Some(name) = channel.channel {
        parts.push(format!("канал {name}"));
    }
    if let Some(dmamux) = channel.dmamux {
        parts.push(format!("через {dmamux}"));
    }
    if let Some(request) = channel.request {
        parts.push(format!("запрос {request}"));
    }
    for remap in channel.remap {
        parts.push(format!("ремап {}.{}", remap.register, remap.field));
    }
    if parts.is_empty() {
        "—".to_owned()
    } else {
        parts.join(", ")
    }
}

/// Предупреждение для вывода, который занят чем-то, кроме периферии.
///
/// Отладочный порт — из зашитой таблицы (в метаданных его нет), выводы кварца
/// и MCO — из самих метаданных: они описаны как сигналы блока `RCC`, и
/// выдумывать для них список не нужно.
fn pin_note(pin: &str) -> Option<String> {
    if let Some((_, note)) = DEBUG_PINS.iter().find(|(name, _)| *name == pin) {
        return Some((*note).to_owned());
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
    Some(note)
}

/// `PA9` → `('A', 9, "")`, чтобы `PA9` шёл перед `PA10`, а `PA0_C` — сразу за
/// `PA0`. Строковая сортировка дала бы `PA10` раньше `PA9`.
/// `TIM10` → `("TIM", 10)`. По той же причине, что и [`pin_sort_key`]:
/// лексикографически `TIM10` встаёт между `TIM1` и `TIM2`, и список блоков
/// читается как случайный.
fn block_sort_key(name: &str) -> (&str, u32) {
    let digits_start = name
        .char_indices()
        .rev()
        .take_while(|(_, char)| char.is_ascii_digit())
        .last()
        .map_or(name.len(), |(index, _)| index);
    (
        &name[..digits_start],
        name[digits_start..].parse().unwrap_or(0),
    )
}

fn pin_sort_key(pin: &str) -> (char, u32, &str) {
    let Some(rest) = pin.strip_prefix('P') else {
        return ('~', 0, pin);
    };
    let mut chars = rest.char_indices();
    let Some((_, port)) = chars.next() else {
        return ('~', 0, pin);
    };
    let digits_end = rest
        .char_indices()
        .skip(1)
        .find(|(_, char)| !char.is_ascii_digit())
        .map_or(rest.len(), |(index, _)| index);
    let number = rest[1..digits_end].parse().unwrap_or(0);
    (port, number, &rest[digits_end..])
}

fn print_header(subject: Option<&str>) {
    match subject {
        Some(subject) => println!("{} ({}) — {subject}", METADATA.name, METADATA.family),
        None => println!("{} ({})", METADATA.name, METADATA.family),
    }
    println!();
}

/// Складывает имена в строки не длиннее `width`, чтобы список блоков читался
/// в терминале, а не уезжал одной строкой на пол-экрана.
fn wrap(names: &[&str], width: usize) -> Vec<String> {
    let mut lines = Vec::new();
    let mut line = String::new();
    for name in names {
        if !line.is_empty() && line.len() + 1 + name.len() > width {
            lines.push(std::mem::take(&mut line));
        }
        if !line.is_empty() {
            line.push(' ');
        }
        line.push_str(name);
    }
    if !line.is_empty() {
        lines.push(line);
    }
    lines
}

/// Проверяет, что чип в `cross/Cargo.toml` — всё ещё тот, под который собран
/// справочник.
///
/// Чип-фича попадает сюда при генерации, а `cross/Cargo.toml` пользователь
/// может поправить руками (сменить чип в существующем проекте — обычное дело).
/// После этого справочник продолжит работать, но показывать будет чужую
/// распиновку — молча, и именно от таких ошибок команда должна защищать.
/// Поэтому предупреждение, а не отказ: данные всё ещё осмысленны, просто не о
/// том чипе.
fn warn_on_chip_drift() {
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| root.join("cross").join("Cargo.toml"));
    let Some(manifest) = manifest else {
        return;
    };
    // Нет файла или не читается — не наше дело: раскладка проекта могла
    // измениться, а справочник от этого не перестаёт быть верным.
    let Ok(text) = fs::read_to_string(&manifest) else {
        return;
    };
    if !text.contains(&format!("\"{CHIP_FEATURE}\"")) {
        eprintln!(
            "ВНИМАНИЕ: в {} нет чип-фичи \"{CHIP_FEATURE}\", под которую собран справочник.\n\
			 Похоже, чип проекта сменили — тогда поправьте его и в chip-info/Cargo.toml,\n\
			 иначе ниже будет распиновка чужого чипа.\n",
            manifest.display(),
        );
    }
}
