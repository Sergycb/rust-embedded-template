//! Справочные запросы: список блоков, описание блока и вывода.

use anyhow::{Context, bail};
use stm32_metapac::metadata::{
    METADATA, Peripheral, PeripheralDmaChannel, PeripheralRccKernelClock,
};

use crate::notes::pin_note;

/// Печатает, с чего начинать: блоки, у которых вообще есть выводы.
///
/// Без аргумента команда именно перечисляет блоки, а не вываливает всю
/// распиновку: имена вроде `SPI1`/`USART2`/`TIM3` угадываются, а вот есть ли
/// на конкретном чипе `SAI1` или `FDCAN2` — нет.
pub(crate) fn list_blocks() {
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
pub(crate) fn describe(query: &str) -> Result<(), anyhow::Error> {
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
pub(crate) fn looks_like_pin(query: &str) -> bool {
    let mut chars = query.chars();
    chars.next() == Some('P')
        && chars.next().is_some_and(|port| port.is_ascii_uppercase())
        && chars.next().is_some_and(|digit| digit.is_ascii_digit())
}

/// Всё, что метаданные знают о привязке канала: на одних семействах это
/// фиксированный канал/поток, на других — номер запроса через DMAMUX, на
/// третьих есть ещё и ремап. Поэтому не разбор по семействам, а перечисление
/// того, что заполнено.
pub(crate) fn format_dma(channel: &PeripheralDmaChannel) -> String {
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

pub(crate) fn print_header(subject: Option<&str>) {
    match subject {
        Some(subject) => println!("{} ({}) — {subject}", METADATA.name, METADATA.family),
        None => println!("{} ({})", METADATA.name, METADATA.family),
    }
    println!();
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
                Some((_, note)) => format!("   ! {note}"),
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

    if let Some((_, note)) = pin_note(name) {
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

/// `PA9` → `('A', 9, "")`, чтобы `PA9` шёл перед `PA10`, а `PA0_C` — сразу за
/// `PA0`. Строковая сортировка дала бы `PA10` раньше `PA9`.
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
