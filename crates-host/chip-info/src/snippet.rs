//! Заготовка кода под блок: `bind_interrupts!` и каркас `assign_resources!`.

use anyhow::Context;
use stm32_metapac::metadata::{METADATA, Peripheral};

use crate::lookup::{format_dma, print_header};
use crate::notes::pin_note;

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

/// Строка таблицы [`HANDLERS`].
///
/// Структура, а не кортеж: полей стало четыре, и два из них — строковые
/// срезы, которые в позиционной записи путаются между собой. Заодно уходит
/// псевдоним, заведённый когда-то ради clippy (`type_complexity`).
struct Handlers {
    /// `kind` блока из метаданных `stm32-metapac`.
    kind: &'static str,
    /// Модуль `embassy_stm32`, в котором живёт обработчик.
    module: &'static str,
    /// Версии блока, у которых обработчик вообще существует. Пустой список —
    /// «у всех версий этого вида».
    ///
    /// Нужно потому, что наличие обработчика в embassy зависит не только от
    /// вида блока: `adc::InterruptHandler` определён лишь для пяти версий
    /// ADC из полутора десятков, а модуль `comp` целиком закрыт
    /// `#[cfg(any(comp_u5, comp_v2))]`. Без фильтра заготовка предлагала бы
    /// тип, которого на этом чипе нет, — и вставивший её получал бы ошибку
    /// компиляции вместо подсказки.
    versions: &'static [&'static str],
    /// Сигнал прерывания → шаблон типа. `{}` заменяется именем блока; без
    /// него идут те, кто в embassy привязан к единственному экземпляру
    /// (`eth`).
    signals: &'static [(&'static str, &'static str)],
}

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
/// Сверено с `embassy-stm32` 0.6. Это подсказка, а не источник правды:
/// последнее слово за компилятором, и при обновлении embassy таблицу стоит
/// перечитать. Блока нет в таблице — значит `bind_interrupts!` ему, скорее
/// всего, не нужен.
///
/// Строки `lpuart` здесь нет намеренно: у метаданных такого вида блока не
/// бывает вовсе, LPUART идёт как `usart` и покрыт первой же строкой
/// (проверено на STM32G071 — заготовка для `LPUART1` печатает
/// `usart::InterruptHandler`).
const HANDLERS: &[Handlers] = &[
    Handlers {
        kind: "usart",
        module: "usart",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    Handlers {
        kind: "i2c",
        module: "i2c",
        versions: &[],
        signals: &[
            ("EV", "EventInterruptHandler<{}>"),
            ("ER", "ErrorInterruptHandler<{}>"),
        ],
    },
    Handlers {
        kind: "can",
        module: "can",
        versions: &[],
        signals: &[
            ("IT0", "IT0InterruptHandler<{}>"),
            ("IT1", "IT1InterruptHandler<{}>"),
            ("TX", "TxInterruptHandler<{}>"),
            ("RX0", "Rx0InterruptHandler<{}>"),
            ("RX1", "Rx1InterruptHandler<{}>"),
            ("SCE", "SceInterruptHandler<{}>"),
        ],
    },
    Handlers {
        kind: "otg",
        module: "usb",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    // У классического USB вектор не один: `LP` — обычные передачи, `HP` —
    // только двойная буферизация и изохронные (драйвер embassy их не
    // включает), `WKUP` — выход из сна. Пока здесь стоял `GLOBAL`, на
    // STM32F103 печаталась ПУСТАЯ `bind_interrupts!` — то есть «привязывать
    // нечего», хотя привязать надо.
    //
    // Только `LP`, без `GLOBAL` «на всякий случай»: embassy реализует
    // `usb::Instance` ровно из ветки `($inst, usb, $block, LP, $irq)` своего
    // `foreach_interrupt!`. На чипе с одним вектором `GLOBAL` типа просто не
    // существовало бы, и заготовка с ним не собралась бы — а лишняя строка
    // здесь ещё и увела бы такой чип мимо диагностики «сигналы не совпали».
    Handlers {
        kind: "usb",
        module: "usb",
        versions: &[],
        signals: &[("LP", "InterruptHandler<{}>")],
    },
    Handlers {
        kind: "rng",
        module: "rng",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    // Тот же блок RNG, но на части чипов stm32-data зовёт его вид `trng`.
    // Модуль embassy при этом всё равно `rng`.
    Handlers {
        kind: "trng",
        module: "rng",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    Handlers {
        kind: "sdmmc",
        module: "sdmmc",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    // Пять версий из полутора десятков: `adc::InterruptHandler` определён
    // только в f1.rs, f3.rs, f3_v1_1.rs и v1.rs, а остальные версии ADC
    // работают без обработчика. Пока фильтра не было, заготовка предлагала
    // несуществующий тип на F407 (v2) и G071 (g0) — то есть на большинстве
    // чипов.
    Handlers {
        kind: "adc",
        module: "adc",
        versions: &["f1", "f3v1", "f3v2", "v1", "l0"],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    // Модуль `comp` в embassy закрыт `#[cfg(any(comp_u5, comp_v2))]`.
    Handlers {
        kind: "comp",
        module: "comp",
        versions: &["u5", "v2"],
        signals: &[("WKUP", "InterruptHandler<{}>")],
    },
    Handlers {
        kind: "dcmi",
        module: "dcmi",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    // `LO`, а не `GLOBAL`: вектора у LTDC два (`LO` — строки/кадры, `ER` —
    // ошибки), и embassy привязывает `Instance::Interrupt` именно к первому —
    // `foreach_interrupt!(($inst, ltdc, LTDC, LO, $irq))` в ltdc.rs. Пока
    // здесь стоял `GLOBAL`, печаталась пустая `bind_interrupts!`.
    Handlers {
        kind: "ltdc",
        module: "ltdc",
        versions: &[],
        signals: &[("LO", "InterruptHandler<{}>")],
    },
    Handlers {
        kind: "quadspi",
        module: "qspi",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    Handlers {
        kind: "hash",
        module: "hash",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    Handlers {
        kind: "cryp",
        module: "cryp",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    Handlers {
        kind: "aes",
        module: "aes",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    Handlers {
        kind: "saes",
        module: "saes",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    Handlers {
        kind: "pka",
        module: "pka",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    Handlers {
        kind: "tsc",
        module: "tsc",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    Handlers {
        kind: "ucpd",
        module: "ucpd",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler<{}>")],
    },
    Handlers {
        kind: "eth",
        module: "eth",
        versions: &[],
        signals: &[("GLOBAL", "InterruptHandler")],
    },
];

/// Заготовка кода под блок: `bind_interrupts!` с правильными типами и каркас
/// `assign_resources!` с реальными именами выводов.
///
/// Печать, а не файл, и это принципиально: генератор распиновки завёл бы
/// второй источник правды к тому, что и так проверяет компилятор (см.
/// doc-комментарий модуля). Здесь же экономится ровно то, на что уходит время
/// руками, — сверка имён сигналов, AF и типов обработчиков с даташитом и
/// исходниками embassy.
pub(crate) fn print_snippet(name: &str) -> Result<(), anyhow::Error> {
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
    let (kind, version) = peripheral
        .registers
        .as_ref()
        .map_or(("", ""), |registers| (registers.kind, registers.version));

    print_header(Some(&format!("{name}: заготовка")));
    println!("// Подсказка: типы и доступность выводов проверит компилятор.");
    println!();

    match HANDLERS.iter().find(|row| {
        row.kind == kind && (row.versions.is_empty() || row.versions.contains(&version))
    }) {
        // Вид блока в таблице есть, а вот ни один её сигнал с сигналами этого
        // чипа не совпал. Печатать пустую `bind_interrupts!` тут нельзя: она
        // читается как «привязывать нечего», хотя на деле разошлись таблица и
        // метаданные. Ровно так молчали классический USB (`LP` против
        // `GLOBAL`) и LTDC (`LO` против `GLOBAL`) — по чипу на каждый случай.
        Some(row) if handlers_by_interrupt(peripheral, row, name).is_empty() => println!(
            "// bind_interrupts! для {name} не собрать: обработчик у блока есть, но сигналы \
             прерываний ({}) не совпали с таблицей HANDLERS в crates-host/chip-info — её надо \
             перечитать по embassy-stm32",
            peripheral
                .interrupts
                .iter()
                .map(|interrupt| interrupt.signal)
                .collect::<Vec<_>>()
                .join(", "),
        ),
        Some(row) => {
            println!("// crates-cross/bsp/src/board.rs — рядом с созданием объектов платы");
            // Импорты печатаются вместе с макросом, а не подразумеваются:
            // заготовку вставляют как есть, и без них она не собирается.
            // `peripherals` нужен не всегда — у `eth` обработчик без
            // параметра.
            let needs_peripherals = row
                .signals
                .iter()
                .any(|(_, handler)| handler.contains("{}"));
            let module = row.module;
            if needs_peripherals {
                println!("use embassy_stm32::{OPEN}bind_interrupts, {module}, peripherals{CLOSE};");
            } else {
                println!("use embassy_stm32::{OPEN}bind_interrupts, {module}{CLOSE};");
            }
            println!();
            println!("bind_interrupts!(struct Irqs {}", OPEN);
            for (interrupt, handlers) in handlers_by_interrupt(peripheral, row, name) {
                println!("    {interrupt} => {};", handlers.join(", "));
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
    println!("// crates-cross/bsp/src/resources.rs — оттуда в макрос переносится только группа");
    // `Peri` — не для красоты: `assign_resources!` раскрывается в
    // `Peri<'static, peripherals::X>` и требует оба имени в области видимости
    // вызова. Без него вставленная заготовка падает с `error[E0425]: cannot
    // find type Peri` — ровно это и случалось, пока импорты подразумевались.
    println!("use assign_resources::assign_resources;");
    println!("use embassy_stm32::{OPEN}Peri, peripherals{CLOSE};");
    println!();
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

/// `USART1` → `Usart1`, `USB_OTG_FS` → `UsbOtgFs`: имя структуры ресурсов в
/// `assign_resources!`.
///
/// Подчёркивания убираются, а не опускаются в нижний регистр: `assign_resources!`
/// глушит `non_snake_case`, но не `non_camel_case_types`, а `cargo xtask lint
/// cross` гоняет clippy с `-D warnings`. Пока здесь оставался `Usb_otg_fs`,
/// вставленная заготовка роняла сборку.
fn camel_case(name: &str) -> String {
    let mut result = String::with_capacity(name.len());
    let mut start_of_word = true;
    for char in name.chars() {
        if char == '_' {
            start_of_word = true;
            continue;
        }
        if start_of_word {
            result.push(char.to_ascii_uppercase());
            start_of_word = false;
        } else {
            result.push(char.to_ascii_lowercase());
        }
    }
    result
}

/// Обработчики блока, сгруппированные по вектору прерывания.
///
/// Группировка — не украшение: на части чипов два сигнала одного блока
/// приходят в ОДИН вектор (у STM32G071 `EV` и `ER` шины I2C1 — это общий
/// `I2C1`), и построчная печать давала два одинаковых ключа, то есть
/// `error[E0428]: the name I2C1 is defined multiple times` во вставленной
/// заготовке. `bind_interrupts!` принимает несколько обработчиков на вектор
/// через запятую — ровно этот случай.
///
/// Порядок — по таблице, а не по метаданным: у I2C сначала `Event`, потом
/// `Error`, как в примерах embassy.
fn handlers_by_interrupt(
    peripheral: &'static Peripheral,
    row: &Handlers,
    name: &str,
) -> Vec<(&'static str, Vec<String>)> {
    let mut grouped: Vec<(&'static str, Vec<String>)> = Vec::new();
    for (signal, template) in row.signals {
        for interrupt in peripheral.interrupts {
            if interrupt.signal != *signal {
                continue;
            }
            let handler = format!(
                "{}::{}",
                row.module,
                template.replace("{}", &format!("peripherals::{name}")),
            );
            match grouped
                .iter_mut()
                .find(|(vector, _)| *vector == interrupt.interrupt)
            {
                Some((_, handlers)) => handlers.push(handler),
                None => grouped.push((interrupt.interrupt, vec![handler])),
            }
        }
    }
    grouped
}
