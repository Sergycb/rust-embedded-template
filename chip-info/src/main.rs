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

/// Фигурные скобки для печатаемых заготовок кода.
///
/// Казалось бы, в `println!` они пишутся удвоением — но этот файл шаблонный,
/// а удвоенная открывающая скобка для cargo-generate уже синтаксис Liquid:
/// генерация проекта падает с «Substitution skipped, found invalid syntax»
/// ещё до компиляции (проверено, причём дважды: второй раз — на этом самом
/// комментарии). Поэтому скобки приезжают аргументами, а писать их подряд
/// нельзя даже в тексте.
const OPEN: char = '{';
const CLOSE: char = '}';

/// Насколько плохо занять вывод под своё.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Severity {
    /// Плата теряет отладку — `--check` завершается ненулевым кодом.
    Conflict,
    /// Зависит от того, как разведена и настроена плата (кварц может быть не
    /// нужен, JTAG в режиме SWD свободен): `--check` печатает и не падает.
    Warning,
}

fn main() -> Result<(), anyhow::Error> {
    warn_on_chip_drift();

    let mut query: Option<String> = None;
    let mut snippet = false;
    let mut check = false;
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--snippet" => snippet = true,
            "--check" => check = true,
            // Пустая строка приезжает из задачи VS Code с пустым ответом на
            // запрос аргумента — см. разбор в xtask.
            "" => {}
            flag if flag.starts_with("--") => bail!(
                "неизвестный ключ {flag}; есть только --snippet (заготовка кода под блок) и \
                 --check (проверить занятые в resources.rs выводы)"
            ),
            value if query.is_none() => query = Some(value.to_owned()),
            extra => bail!("лишний аргумент {extra}: блок или вывод указывается один"),
        }
    }

    if check {
        anyhow::ensure!(
            query.is_none() && !snippet,
            "--check работает сам по себе: он смотрит не на один блок, а на всю распиновку в \
             cross/bsp/src/resources.rs",
        );
        return check_resources();
    }

    match (query, snippet) {
        (None, false) => {
            list_blocks();
            Ok(())
        }
        (None, true) => bail!("--snippet нужен блок: cargo xtask pins USART1 --snippet"),
        (Some(query), false) => describe(&query),
        (Some(query), true) => print_snippet(&query.to_uppercase()),
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

/// Строка таблицы [`HANDLERS`]: `kind` из метаданных, имя модуля
/// `embassy_stm32` и пары «сигнал прерывания → шаблон типа обработчика».
///
/// Псевдоним, а не голый кортеж прямо в типе константы, по требованию clippy
/// (`type_complexity` под `-D warnings`).
type HandlerTable = (
    &'static str,
    &'static str,
    &'static [(&'static str, &'static str)],
);

/// Модуль `embassy_stm32` и типы обработчиков прерываний для блока: `kind` из
/// метаданных совпадает с именем модуля почти всегда (`usart`, `i2c`, `rng`,
/// `sdmmc`), а сигнал прерывания различает обработчики там, где их несколько.
///
/// Зачем таблица, а не догадка по имени блока: у `I2C` обработчика два
/// (`EventInterruptHandler` и `ErrorInterruptHandler`), у FDCAN — свои
/// `IT0`/`IT1`, у bxCAN — четыре, а у `SPI` и таймеров их нет вовсе (работают
/// через DMA, `bind_interrupts!` им не нужен). Компилятор про ошибку скажет,
/// но уже после того, как вы полчаса выясняете, какой именно тип он ждёт.
///
/// Третье поле — шаблон типа; `{}` в нём заменяется именем блока. Без
/// параметра идут те, кто в embassy привязан к единственному экземпляру
/// (`eth`).
///
/// Сверено с `embassy-stm32` 0.6. Это подсказка, а не источник правды:
/// последнее слово за компилятором, и при обновлении embassy таблицу стоит
/// перечитать. Блока нет в таблице — значит `bind_interrupts!` ему, скорее
/// всего, не нужен.
const HANDLERS: &[HandlerTable] = &[
    ("usart", "usart", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("lpuart", "usart", &[("GLOBAL", "InterruptHandler<{}>")]),
    (
        "i2c",
        "i2c",
        &[
            ("EV", "EventInterruptHandler<{}>"),
            ("ER", "ErrorInterruptHandler<{}>"),
        ],
    ),
    (
        "can",
        "can",
        &[
            ("IT0", "IT0InterruptHandler<{}>"),
            ("IT1", "IT1InterruptHandler<{}>"),
            ("TX", "TxInterruptHandler<{}>"),
            ("RX0", "Rx0InterruptHandler<{}>"),
            ("RX1", "Rx1InterruptHandler<{}>"),
            ("SCE", "SceInterruptHandler<{}>"),
        ],
    ),
    ("otg", "usb", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("usb", "usb", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("rng", "rng", &[("GLOBAL", "InterruptHandler<{}>")]),
    // Тот же блок RNG, но на части чипов stm32-data зовёт его вид `trng`.
    // Модуль embassy при этом всё равно `rng`.
    ("trng", "rng", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("sdmmc", "sdmmc", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("adc", "adc", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("dcmi", "dcmi", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("ltdc", "ltdc", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("quadspi", "qspi", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("hash", "hash", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("cryp", "cryp", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("aes", "aes", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("saes", "saes", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("pka", "pka", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("tsc", "tsc", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("ucpd", "ucpd", &[("GLOBAL", "InterruptHandler<{}>")]),
    ("eth", "eth", &[("GLOBAL", "InterruptHandler")]),
];

/// Заготовка кода под блок: `bind_interrupts!` с правильными типами и каркас
/// `assign_resources!` с реальными именами выводов.
///
/// Печать, а не файл, и это принципиально: генератор `resources.rs` завёл бы
/// второй источник правды к тому, что и так проверяет компилятор (см.
/// doc-комментарий модуля). Здесь же экономится ровно то, на что уходит время
/// руками, — сверка имён сигналов, AF и типов обработчиков с даташитом и
/// исходниками embassy.
fn print_snippet(name: &str) -> Result<(), anyhow::Error> {
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
    let kind = peripheral
        .registers
        .as_ref()
        .map_or("", |registers| registers.kind);

    print_header(Some(&format!("{name}: заготовка")));
    println!("// Подсказка: типы и доступность выводов проверит компилятор.");
    println!();

    match HANDLERS
        .iter()
        .find(|(table_kind, _, _)| *table_kind == kind)
    {
        Some((_, module, handlers)) => {
            println!("// cross/app/src/main.rs — рядом с созданием периферии");
            println!("bind_interrupts!(struct Irqs {}", OPEN);
            for interrupt in peripheral.interrupts {
                let Some((_, handler)) = handlers
                    .iter()
                    .find(|(signal, _)| *signal == interrupt.signal)
                else {
                    continue;
                };
                println!(
                    "    {} => {module}::{};",
                    interrupt.interrupt,
                    handler.replace("{}", &format!("peripherals::{name}")),
                );
            }
            println!("{});", CLOSE);
        }
        // Не «данных не хватило», а содержательный ответ: у SPI и таймеров
        // обработчиков в embassy нет, они работают через DMA.
        None => println!(
            "// bind_interrupts! этому блоку не нужен (в embassy у него нет обработчика прерывания)"
        ),
    }

    println!();
    println!("// cross/bsp/src/resources.rs");
    println!("assign_resources! {}", OPEN);
    println!(
        "    {}: {}Resources {}",
        name.to_lowercase(),
        camel_case(name),
        OPEN,
    );
    println!("        {}: {name},", kind_field(kind, name));
    let fields = signals(peripheral);
    let width = fields
        .iter()
        .map(|(signal, default, _)| signal.len() + default.len())
        .max()
        .unwrap_or(0);
    for (signal, default, alternatives) in &fields {
        let field = format!("{}: {default},", signal.to_lowercase());
        // Комментарии в колонку: заготовку читают глазами, и разъезжающиеся
        // «ещё:» мешают сравнить, у какого сигнала выбор шире.
        let padding = " ".repeat(width + 2 - (signal.len() + default.len()));
        let comment = match (alternatives.is_empty(), pin_note(default)) {
            (true, None) => String::new(),
            (false, None) => format!("{padding}// ещё: {}", alternatives.join(" ")),
            (true, Some((_, note))) => format!("{padding}// ! {note}"),
            (false, Some((_, note))) => {
                format!("{padding}// ! {note}; ещё: {}", alternatives.join(" "))
            }
        };
        println!("        {field}{comment}");
    }
    for channel in peripheral.dma_channels {
        println!(
            "        // DMA {}: {} — подставьте свободный канал",
            channel.signal.to_lowercase(),
            format_dma(channel),
        );
    }
    println!("    {}", CLOSE);
    println!("{}", CLOSE);
    Ok(())
}

/// Имя поля под сам блок в `assign_resources!`: `usart`, `i2c`, `spi` —
/// то есть `kind`, а не `usart1`, чтобы код задачи не переписывался при
/// переезде на другой экземпляр.
fn kind_field<'a>(kind: &'a str, name: &'a str) -> &'a str {
    if kind.is_empty() { name } else { kind }
}

/// Сигналы блока с их выводами, отсортированные так, чтобы первым в списке
/// стоял свободный вывод (не отладочный порт и не кварц).
fn signals(peripheral: &Peripheral) -> Vec<(&'static str, String, Vec<String>)> {
    // Свободные и служебные копятся раздельно, чтобы служебные оказались в
    // конце: исключать их нельзя (на конкретной плате кварца может не быть, и
    // тогда его выводы — законная опция), но и подставлять PA13 в заготовку
    // было бы вредным советом.
    let mut signals: Vec<(&'static str, Vec<&'static str>, Vec<&'static str>)> = Vec::new();
    let mut af_of: Vec<(&'static str, &'static str, Option<u8>)> = Vec::new();
    for pin in peripheral.pins {
        let entry = match signals
            .iter_mut()
            .find(|(signal, _, _)| *signal == pin.signal)
        {
            Some(entry) => entry,
            None => {
                signals.push((pin.signal, Vec::new(), Vec::new()));
                signals.last_mut().expect("только что добавили")
            }
        };
        if pin_note(pin.pin).is_some() {
            entry.2.push(pin.pin);
        } else {
            entry.1.push(pin.pin);
        }
        af_of.push((pin.signal, pin.pin, pin.af));
    }
    signals.sort_by_key(|(signal, _, _)| *signal);

    let mut fields = Vec::new();
    for (signal, mut free, busy) in signals {
        free.extend(busy);
        let Some((default, alternatives)) = free.split_first() else {
            continue;
        };
        // AF нужен только в комментарии со списком альтернатив: значением
        // поля идёт голое имя вывода, иначе заготовка не компилируется.
        let alternatives = alternatives
            .iter()
            .map(|pin| {
                match af_of
                    .iter()
                    .find(|(candidate_signal, candidate_pin, _)| {
                        *candidate_signal == signal && candidate_pin == pin
                    })
                    .and_then(|(_, _, af)| *af)
                {
                    Some(af) => format!("{pin}(AF{af})"),
                    None => (*pin).to_owned(),
                }
            })
            .collect();
        fields.push((signal, (*default).to_owned(), alternatives));
    }
    fields
}

/// `USART1` → `Usart1`: имя структуры ресурсов в `assign_resources!`.
fn camel_case(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    for (index, char) in name.chars().enumerate() {
        if index == 0 {
            result.push(char);
        } else {
            result.push(char.to_ascii_lowercase());
        }
    }
    result
}

/// Проверяет распиновку проекта на то, чего компилятор не видит: занятый
/// отладочный порт и выводы кварца.
///
/// Всё остальное про распиновку он и так ловит — чужой сигнал на выводе
/// (трейт не реализован), один вывод в двух задачах (`Peri` не `Copy`), вывод
/// не из корпуса (чип-фича пакетная). А вот что вы отрезали себе `probe-rs`,
/// заняв `PA13`, компилятор сказать не может: с точки зрения типов это
/// обычный GPIO.
///
/// Разбор текстовый, без `syn`: `assign_resources!` — макрос, и его содержимое
/// до раскрытия не дерево, а токены; выискивать в них имена выводов сложнее,
/// чем прочитать файл построчно. Цена — комментарии приходится пропускать
/// вручную (иначе примеры из doc-комментария считались бы занятыми выводами).
fn check_resources() -> Result<(), anyhow::Error> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(|root| {
            root.join("cross")
                .join("bsp")
                .join("src")
                .join("resources.rs")
        })
        .context("не найден корень проекта рядом с chip-info")?;
    let text =
        fs::read_to_string(&path).with_context(|| format!("не читается {}", path.display()))?;

    print_header(Some("проверка распиновки"));
    println!("файл: {}", path.display());

    let mut used: Vec<(usize, String)> = Vec::new();
    for (number, line) in text.lines().enumerate() {
        // Комментарий отрезается, а не только пропускается целиком: заготовка,
        // которую печатает `pins БЛОК --snippet`, перечисляет альтернативные
        // выводы прямо в хвосте строки (`sck: PA5,  // ещё: PB3(AF5)`), и без
        // этого вставленная заготовка давала бы ложный конфликт.
        let line = line.split("//").next().unwrap_or("");
        for pin in pins_in(line) {
            if METADATA.pins.iter().any(|known| known.name == pin) {
                used.push((number + 1, pin));
            }
        }
    }

    if used.is_empty() {
        println!();
        println!("выводов не назначено — распиновка ещё не заполнена");
        return Ok(());
    }

    println!("назначено выводов: {}", used.len());
    println!();

    let mut conflicts = 0;
    for (number, pin) in &used {
        let Some((severity, note)) = pin_note(pin) else {
            continue;
        };
        let label = match severity {
            Severity::Conflict => {
                conflicts += 1;
                "КОНФЛИКТ"
            }
            Severity::Warning => "внимание",
        };
        println!("{label}: {pin} (строка {number}) — {note}");
    }

    if conflicts == 0 {
        println!("отладочный порт не занят");
        return Ok(());
    }
    bail!(
        "занято выводов отладочного порта: {conflicts} — после прошивки плата перестанет \
         отвечать пробнику, вытаскивать придётся через `probe-rs erase --connect-under-reset`"
    )
}

/// Имена выводов в строке кода: `P` + буква порта + цифры, не внутри другого
/// идентификатора (иначе `SPI1` читалось бы как вывод `PI1`).
fn pins_in(line: &str) -> Vec<String> {
    let bytes = line.as_bytes();
    let mut found = Vec::new();
    for (index, _) in line.match_indices('P') {
        let previous_is_ident = index
            .checked_sub(1)
            .is_some_and(|before| bytes[before].is_ascii_alphanumeric() || bytes[before] == b'_');
        if previous_is_ident {
            continue;
        }
        let rest = &line[index..];
        let end = rest
            .char_indices()
            .position(|(offset, char)| offset > 0 && !char.is_ascii_alphanumeric() && char != '_')
            .unwrap_or(rest.len());
        let candidate = &rest[..end];
        if looks_like_pin(candidate) {
            found.push(candidate.to_owned());
        }
    }
    found
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
fn pin_note(pin: &str) -> Option<(Severity, String)> {
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
