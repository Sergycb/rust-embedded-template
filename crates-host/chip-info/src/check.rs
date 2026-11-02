//! `--check`: занятые распиновкой выводы, которых не видит компилятор.

use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};
use stm32_metapac::metadata::METADATA;

use crate::lookup::{looks_like_pin, print_header};
use crate::notes::{Severity, pin_note};

/// Корень проекта — два уровня вверх от манифеста: сам крейт лежит в
/// `crates-host/chip-info`.
///
/// От `CARGO_MANIFEST_DIR`, а не от текущего каталога: команду зовут через
/// `cargo xtask pins` откуда угодно, и `current_dir()` указывал бы куда
/// попало. Манифест берётся из среды запуска (`cargo run` её задаёт), а
/// `env!` — только запасной путь: перенесённый проект cargo не пересобирает,
/// и вшитый при сборке путь вёл бы в старый каталог (так же `root_dir` в
/// xtask).
pub(crate) fn project_root() -> Option<PathBuf> {
    env::var_os("CARGO_MANIFEST_DIR")
        .map_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")), PathBuf::from)
        .parent()
        .and_then(Path::parent)
        .map(Path::to_path_buf)
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
pub(crate) fn check_resources() -> Result<(), anyhow::Error> {
    let root = project_root().context("не найден корень проекта рядом с chip-info")?;
    let src = root.join("crates-cross").join("bsp").join("src");
    let sources = resource_sources(&src)?;

    print_header(Some("проверка распиновки"));
    println!("файлы: {}", src.display());

    let mut used: Vec<(String, usize, String)> = Vec::new();
    for path in &sources {
        let text =
            fs::read_to_string(path).with_context(|| format!("не читается {}", path.display()))?;
        // Путь от `bsp/src`, а не одно имя файла: с рекурсивным обходом
        // `board/pins.rs` и `pins.rs` иначе выглядели бы одинаково.
        let name = path
            .strip_prefix(&src)
            .unwrap_or(path.as_path())
            .display()
            .to_string();
        for (number, line) in assigned_lines(&text) {
            for pin in pins_in(&line) {
                if METADATA.pins.iter().any(|known| known.name == pin) {
                    used.push((name.clone(), number, pin));
                }
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
    for (file, number, pin) in &used {
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
        println!("{label}: {pin} ({file}, строка {number}) — {note}");
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

/// Строки тела `assign_resources!` без комментариев — то есть ровно то, что
/// действительно назначает выводы, с номерами строк исходного файла.
///
/// Разбор текстом, а не по дереву: `assign_resources!` — макрос, до раскрытия
/// там токены (см. шапку модуля). Но текстом надо разбирать честно, иначе
/// проверка врёт, причём в сторону ложной тревоги:
///
/// * вырезался только `//`, поэтому вывод внутри `/* ... */` считался
///   назначенным;
/// * считался весь файл, поэтому обычный
///   `use embassy_stm32::peripherals::PA13;` давал «КОНФЛИКТ: PA13» и ронял
///   джоб `Pins` на проекте, который отладочный порт не трогал вовсе.
///
/// Хвостовой комментарий при этом по-прежнему отрезается, а не пропускает
/// строку целиком: заготовка из `pins БЛОК --snippet` перечисляет
/// альтернативные выводы прямо за запятой (`sck: PA5,  // ещё: PB3(AF5)`), и
/// они назначенными не являются.
///
/// Глубина блочных комментариев считается, а не сбрасывается первым же `*/`:
/// в Rust они вкладываются.
fn assigned_lines(text: &str) -> Vec<(usize, String)> {
    let mut lines = Vec::new();
    let mut comment_depth = 0usize;
    let mut brace_depth = 0usize;
    let mut inside_macro = false;
    // Тело считается закрытым только после того, как хоть одна скобка
    // открылась: иначе `assign_resources!` с `{` на следующей строке закрывал
    // бы блок сразу же, ещё до первого вывода.
    let mut braces_opened = false;

    for (index, raw) in text.lines().enumerate() {
        let chars: Vec<char> = raw.chars().collect();
        let mut cleaned = String::with_capacity(raw.len());
        let mut at = 0;
        while at < chars.len() {
            let pair = (chars[at], chars.get(at + 1).copied());
            match pair {
                ('/', Some('*')) => {
                    comment_depth += 1;
                    at += 2;
                }
                ('*', Some('/')) if comment_depth > 0 => {
                    comment_depth -= 1;
                    at += 2;
                }
                ('/', Some('/')) if comment_depth == 0 => break,
                _ => {
                    if comment_depth == 0 {
                        cleaned.push(chars[at]);
                    }
                    at += 1;
                }
            }
        }

        if !inside_macro {
            if !cleaned.contains("assign_resources!") {
                continue;
            }
            inside_macro = true;
            brace_depth = 0;
            braces_opened = false;
        }

        let opened = cleaned.matches('{').count();
        braces_opened |= opened > 0;
        brace_depth += opened;
        brace_depth = brace_depth.saturating_sub(cleaned.matches('}').count());
        lines.push((index + 1, cleaned));
        if braces_opened && brace_depth == 0 {
            inside_macro = false;
        }
    }
    lines
}

/// Исходники `bsp`, в которых ищется распиновка.
///
/// Каталогом, а не одним файлом: шаблон кладёт `assign_resources!` в
/// `resources.rs`, но держать его там проект не обязан, поэтому проверяются
/// все.
///
/// Обход рекурсивный, и это не запас на будущее: `assign_resources!` в
/// `bsp/src/board/pins.rs` пропущенным быть не может — молчаливое «выводов не
/// назначено» на занятом `PA13` хуже отсутствия проверки, потому что выглядит
/// как её прохождение.
///
/// Сортировка — чтобы порядок вывода не зависел от файловой системы: без неё
/// один и тот же проект печатал бы находки в разном порядке на разных
/// машинах.
fn resource_sources(src: &Path) -> Result<Vec<PathBuf>, anyhow::Error> {
    let mut files = Vec::new();
    let mut dirs = vec![src.to_path_buf()];
    while let Some(dir) = dirs.pop() {
        for entry in fs::read_dir(&dir).with_context(|| format!("не читается {}", dir.display()))?
        {
            let path = entry?.path();
            if path.is_dir() {
                dirs.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files.sort();
    Ok(files)
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

#[cfg(test)]
mod tests {
    use std::{env, fs, path::PathBuf, process};

    use super::{assigned_lines, pins_in, resource_sources};

    /// Выводы, которые `check_resources` сочтёт назначенными: строки тела
    /// `assign_resources!` без комментариев, разобранные на имена выводов.
    fn assigned_pins(text: &str) -> Vec<(usize, String)> {
        assigned_lines(text)
            .into_iter()
            .flat_map(|(number, line)| pins_in(&line).into_iter().map(move |pin| (number, pin)))
            .collect()
    }

    fn pins(found: &[(usize, String)]) -> Vec<&str> {
        found.iter().map(|(_, pin)| pin.as_str()).collect()
    }

    /// Обычный `resources.rs`: назначенные выводы находятся с номерами строк
    /// исходного файла, а `//!`, импорты и код за макросом не в счёт.
    #[test]
    fn finds_pins_assigned_in_the_macro_body() {
        let text = "\
//! Пример из документации: `swdio: PA13` здесь не назначение.
use embassy_stm32::{Peri, peripherals};

assign_resources! {
    flash: FlashResources { flash: FLASH },
    usart: UsartResources {
        usart: USART1,
        tx: PA9,
        rx: PA10,
    },
}

fn after() -> u8 { PA14 }
";
        assert_eq!(
            assigned_pins(text),
            [(8, "PA9".to_owned()), (9, "PA10".to_owned())]
        );
    }

    /// Регрессия: вырезался только `//`, и вывод внутри `/* ... */` считался
    /// назначенным — в том числе когда комментарий тянется через строки.
    #[test]
    fn skips_pins_inside_block_comments() {
        let text = "\
assign_resources! {
    usart: UsartResources { usart: USART1, /* swdio: PA13, */ tx: PA9 },
    /*
    debug: DebugResources { swclk: PA14 },
    */
}
";
        assert_eq!(pins(&assigned_pins(text)), ["PA9"]);
    }

    /// Регрессия: считался весь файл, и обычный импорт `PA13` давал
    /// «КОНФЛИКТ» на проекте, который отладочный порт не трогал.
    #[test]
    fn ignores_pins_outside_assign_resources() {
        let text = "\
use embassy_stm32::peripherals::PA13;

assign_resources! {
    usart: UsartResources { usart: USART1, tx: PA9 },
}
";
        assert_eq!(pins(&assigned_pins(text)), ["PA9"]);
    }

    /// Регрессия: первый же `*/` закрывал комментарий целиком, хотя в Rust
    /// блочные комментарии вкладываются, — и остаток внешнего считался кодом.
    #[test]
    fn counts_nested_block_comment_depth() {
        let text = "\
assign_resources! {
    /* внешний /* внутренний: PA13 */ всё ещё комментарий: PA14 */
    usart: UsartResources { usart: USART1, tx: PA9 },
}
";
        assert_eq!(pins(&assigned_pins(text)), ["PA9"]);
    }

    /// Регрессия: `SPI1` читался как вывод `PI1` — имя вывода внутри другого
    /// идентификатора выводом не является.
    #[test]
    fn does_not_read_a_pin_inside_an_identifier() {
        assert_eq!(pins_in("spi: SPI1, sck: PA5, mosi: PA7"), ["PA5", "PA7"]);
        assert_eq!(pins_in("dma: DMA1_CH4, tim: TIM_PI1"), Vec::<String>::new());
        // Сам по себе `PI1` — вывод, и суффикс двойника тоже часть имени.
        assert_eq!(pins_in("a: PI1, b: PA0_C"), ["PI1", "PA0_C"]);
    }

    /// Хвостовой комментарий отрезается, а строка остаётся: заготовка из
    /// `pins БЛОК --snippet` перечисляет альтернативы прямо за запятой, и
    /// назначен из них только первый вывод.
    #[test]
    fn cuts_the_trailing_comment_but_keeps_the_line() {
        let text = "\
assign_resources! {
    spi: SpiResources {
        spi: SPI1,
        sck: PA5,  // ещё: PB3(AF5)
        /// Doc-комментарий в теле: miso: PA13
        miso: PA6,
    },
}
";
        assert_eq!(pins(&assigned_pins(text)), ["PA5", "PA6"]);
    }

    /// Тело закрывается только после первой открытой скобки: с `{` на
    /// следующей строке макрос не кончается на самом `assign_resources!`.
    #[test]
    fn opens_the_body_on_the_next_line() {
        let text = "\
assign_resources!
{
    usart: UsartResources { usart: USART1, tx: PA9 },
}
static AFTER: u8 = PA13;
";
        assert_eq!(pins(&assigned_pins(text)), ["PA9"]);
    }

    /// Обход рекурсивный: `assign_resources!` в `board/pins.rs` не теряется —
    /// иначе занятый `PA13` прошёл бы проверку как «выводов не назначено».
    /// Порядок от файловой системы не зависит.
    #[test]
    fn collects_rust_sources_recursively_and_sorted() {
        let root = env::temp_dir().join(format!("chip-info-sources-{}", process::id()));
        if root.exists() {
            fs::remove_dir_all(&root).unwrap();
        }
        fs::create_dir_all(root.join("board")).unwrap();
        for file in ["resources.rs", "board.rs", "board/pins.rs", "memory.x"] {
            fs::write(root.join(file), "").unwrap();
        }

        let found = resource_sources(&root).unwrap();
        fs::remove_dir_all(&root).unwrap();

        let relative: Vec<_> = found
            .iter()
            .map(|path| path.strip_prefix(&root).unwrap().to_path_buf())
            .collect();
        assert_eq!(
            relative,
            [
                ["board", "pins.rs"].iter().collect(),
                PathBuf::from("board.rs"),
                PathBuf::from("resources.rs"),
            ]
        );
    }
}
