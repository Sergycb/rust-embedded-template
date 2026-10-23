//! Разбор `MEMORY {}` из `memory.x` приложения.

use std::{fs, path::Path};

use anyhow::Context;

use crate::project::root_dir;

/// Регион из `MEMORY {}`: `build` по нему меряет раздел приложения,
/// `test host-target` передаёт тесту адреса разделов OTA, `panic-info` читает PANIC.
pub struct Region {
    pub origin: u64,
    /// Нужна `panic-info`: по ней проверяется, что записанная в дампе длина
    /// сообщения вообще помещается в регион.
    pub length: u64,
}

/// Регионы из `memory.x` приложения.
///
/// Один разбор на три места (`build`, `test host-target`, `panic-info`): путь и
/// текст ошибки уже были скопированы, и следующая правка раскладки нашлась бы
/// не во всех копиях.
pub fn app_memory_regions() -> Result<Vec<(String, Region)>, anyhow::Error> {
    let memory_x = root_dir().join("crates-cross").join("app").join("memory.x");
    parse_memory_regions(&memory_x).with_context(|| format!("разобрать {}", memory_x.display()))
}

/// Регионы из `memory.x` bootloader'а: его `FLASH` — собственная зона до
/// `BOOTLOADER_STATE`. Только при `has_bootloader()`: без OTA каталога нет.
pub fn boot_memory_regions() -> Result<Vec<(String, Region)>, anyhow::Error> {
    let memory_x = root_dir()
        .join("crates-cross")
        .join("boot")
        .join("memory.x");
    parse_memory_regions(&memory_x).with_context(|| format!("разобрать {}", memory_x.display()))
}

/// Раздел, в который линкуется приложение, и как его называть в выводе.
///
/// Приложение линкуется в `ACTIVE`, а если такого региона нет — в `FLASH`.
/// Причём в посчитанном при генерации файле `FLASH` приложения это и ЕСТЬ
/// раздел `ACTIVE`: линкеру он отдан под этим именем. Отсюда и второе имя в
/// паре — по наличию `DFU` видно, раздел перед нами или весь флеш, а сказать
/// «из 128 KiB FLASH» про чип с 512 KiB значило бы соврать.
pub fn app_region(regions: &[(String, Region)]) -> Option<(&'static str, &Region)> {
    let partitioned = region(regions, "DFU").is_some();
    region(regions, "ACTIVE")
        .map(|active| ("ACTIVE", active))
        .or_else(|| {
            region(regions, "FLASH")
                .map(|flash| (if partitioned { "ACTIVE" } else { "FLASH" }, flash))
        })
}

/// `MEMORY { NAME (attrs) : ORIGIN = 0x..., LENGTH = 128K }` из `memory.x`.
/// Свой разбор, а не крейт: формат пишем мы сами (`chip-select.rhai`), а
/// нужны из него только адрес и размер.
///
/// **Нераспознанная строка региона — ошибка, а не пропуск.** Раньше здесь стояло
/// пять `continue` подряд, и любая форма, которую парсер не знал, превращалась
/// в «региона нет»: `build` писал `.bin` и подписывал его, не сверив ни базу, ни
/// размер, `test host-target` молча пропускал цикл OTA, а `panic-info` отвечал, что
/// дампа нет. Такие формы линкер принимает — то есть отказ был не «сломан
/// файл», а «тихо потеряны все проверки». Разбирается ровно то, что пишет
/// шаблон, всё прочее падает с текстом строки.
///
/// Незаполненный шаблон (`crates-cross/app/memory.x` в репозитории шаблона, где
/// значения стоят внутри `/* … */`) — тоже ошибка, но с другим текстом: линкер
/// такой файл не собирает вовсе, и оставить проект без `memory.x` молча всё
/// равно нельзя.
pub fn parse_memory_regions(memory_x: &Path) -> Result<Vec<(String, Region)>, anyhow::Error> {
    let text = fs::read_to_string(memory_x)?;
    let mut regions = Vec::new();
    let mut in_comment = false;
    for line in text.lines() {
        let line = line.trim();
        // Внутри многострочного `/* … */` строка вида `FLASH (rx) : ORIGIN = …` —
        // закомментированный старый вариант, а не регион: иначе он перекрыл бы
        // настоящий (`region()` берёт первое совпадение по имени).
        let inside_comment = in_comment;
        in_comment = comment_open_after(line, in_comment);
        if inside_comment || !is_region_line(line) {
            continue;
        }
        regions.push(parse_region_line(line, memory_x)?);
    }
    Ok(regions)
}

/// Строка региона — это `NAME (attrs) : …`, и больше ничего: скобка с
/// атрибутами и двоеточие сразу за ней.
///
/// Отсекает всё остальное содержимое файла — `MEMORY {`, `}`, комментарии и
/// присваивания символов (`__flash_base = 0x08000000;`), у которых скобки есть,
/// а двоеточия после них нет.
fn is_region_line(line: &str) -> bool {
    // `chip-data-gen` комментирует регионы, которых у чипа нет физически
    // (`/* NAME (rw) : ORIGIN=…, LENGTH=1K - причина */`): это не регион.
    if line.starts_with("/*") {
        return false;
    }
    let Some((head, values)) = line.split_once(':') else {
        return false;
    };
    let name = head.split('(').next().unwrap_or("").trim();
    let has_attrs = head.contains('(') && head.trim_end().ends_with(')');
    // Атрибуты линкер допускает опустить (`ACTIVE : ORIGIN = …`), поэтому без
    // них строка считается регионом по ключевым словам значения, включая
    // сокращения `ORG`/`LEN`: молчаливо отбросить такой регион — то самое
    // «потеряли все проверки».
    !name.is_empty()
        && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_')
        && (has_attrs
            || ["ORIGIN", "LENGTH", "ORG", "LEN"]
                .iter()
                .any(|k| values.contains(k)))
}

/// Остаётся ли блочный комментарий открытым к концу строки.
fn comment_open_after(line: &str, mut open: bool) -> bool {
    let mut rest = line;
    loop {
        let marker = if open { "*/" } else { "/*" };
        match rest.find(marker) {
            Some(at) => {
                rest = &rest[at + 2..];
                open = !open;
            }
            None => return open,
        }
    }
}

/// Разбирает одну строку региона. Возвращает пару `(имя, Region)` или ошибку с
/// этой строкой в сообщении.
fn parse_region_line(line: &str, source: &Path) -> Result<(String, Region), anyhow::Error> {
    let unparsed = || {
        format!(
            "{}: не удалось разобрать строку региона: {line}\nОжидается форма \
             `NAME (attrs) : ORIGIN = <число>, LENGTH = <число>` — та, что пишет \
             chip-select.rhai. Другая форма (ORIGIN(SYMBOL), разбитая на две строки) молча \
             лишила бы образ всех проверок",
            source.display(),
        )
    };
    let parsed = (|| {
        let (head, values) = line.split_once(':')?;
        let name = head.split('(').next()?;
        let (origin, length) = values.split_once("LENGTH")?;
        let origin = origin.split_once("ORIGIN").map_or("", |(_, value)| value);
        Some((name, origin, length))
    })();
    let Some((name, origin, length)) = parsed else {
        anyhow::bail!(unparsed());
    };

    // Комментарий убирается у значения, а не у всей строки: в незаполненном
    // шаблоне `/* … */` стоит ВМЕСТО значения, и обрезание всей строки унесло
    // бы вместе с ним ключевое слово `LENGTH` — заставив сообщать о «форме,
    // которую не удалось разобрать», вместо «значение в файле не заполнено».
    #[expect(
        clippy::items_after_statements,
        reason = "вспомогательная функция стоит рядом с использованием"
    )]
    fn region_value(raw: &str) -> &str {
        let raw = raw.split_once("/*").map_or(raw, |(before, _)| before);
        raw.trim_start_matches([' ', '=', ',']).trim()
    }
    let (origin, length) = (region_value(origin), region_value(length));
    if origin.is_empty() || length.is_empty() {
        anyhow::bail!(
            "{}: строка региона не заполнена, значение стоит в /* … */: {line}\nРаскладку \
             считает chip-data-gen при генерации; если она не вышла, регионы вписывают руками \
             по образцу в crates-cross/app/memory.x",
            source.display(),
        );
    }
    match (parse_size(origin), parse_size(length)) {
        (Some(origin), Some(length)) => Ok((name.trim().to_owned(), Region { origin, length })),
        _ => anyhow::bail!(
            "{}: не разобраны значения региона ({origin:?}, {length:?}) в строке: {line}\nАдрес \
             и длина пишутся числом: 0x08020000, 128K, 1M или 528",
            source.display(),
        ),
    }
}

/// `128K`, `1M`, `528`, `0x08020000` — то, чем линкерный скрипт записывает и
/// размеры, и адреса.
fn parse_size(raw: &str) -> Option<u64> {
    let raw = raw.trim().trim_end_matches(',').trim();
    if let Some(hex) = raw.strip_prefix("0x").or_else(|| raw.strip_prefix("0X")) {
        return u64::from_str_radix(hex, 16).ok();
    }
    let (digits, multiplier) = match raw.chars().last()? {
        'K' | 'k' => (&raw[..raw.len() - 1], 1024),
        'M' | 'm' => (&raw[..raw.len() - 1], 1024 * 1024),
        _ => (raw, 1),
    };
    digits
        .trim()
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(multiplier))
}

pub fn region<'a>(regions: &'a [(String, Region)], name: &str) -> Option<&'a Region> {
    regions
        .iter()
        .find(|(region_name, _)| region_name == name)
        .map(|(_, region)| region)
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::scratch::scratch;

    /// Закомментированный многострочным блоком старый вариант региона не должен
    /// перекрывать настоящий.
    #[test]
    fn ignores_regions_inside_a_multiline_comment() {
        let dir = scratch("multiline-comment");
        let path = dir.path().join("memory.x");
        fs::write(
            &path,
            "MEMORY {\n/* старый вариант:\n   FLASH (rx) : ORIGIN = 0x08000000, LENGTH = 1M\n*/\n\
             FLASH (rx) : ORIGIN = 0x08010000, LENGTH = 64K\n}\n",
        )
        .expect("записать memory.x");
        let regions = super::parse_memory_regions(&path).expect("разобрать");
        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].1.origin, 0x0801_0000);
    }

    /// Атрибуты в MEMORY необязательны: такая форма — регион, а не мусор.
    #[test]
    fn parses_a_region_without_attributes() {
        let dir = scratch("no-attrs");
        let path = dir.path().join("memory.x");
        fs::write(
            &path,
            "MEMORY {\n    ACTIVE : ORIGIN = 0x08000000, LENGTH = 64K\n}\n",
        )
        .expect("записать memory.x");
        let regions = super::parse_memory_regions(&path).expect("разобрать");
        assert_eq!(regions[0].0, "ACTIVE");
        assert_eq!(regions[0].1.length, 64 * 1024);
    }

    /// Раскладка в том виде, в каком её пишет `chip-select.rhai`, плюс строки,
    /// которые рядом с ней обязаны быть и которые не должны попасть в разбор:
    /// присваивания символов содержат скобки, а комментарий в конце строки
    /// региона — это валидный для линкера текст.
    #[test]
    fn parses_the_generated_layout_and_ignores_the_rest() {
        let dir = scratch("layout");
        let path = dir.path().join("memory.x");
        fs::write(
            &path,
            "\
/* Сгенерировано chip-select.rhai */

MEMORY {
    FLASH             (rx)  : ORIGIN = 0x08020000, LENGTH = 128K
    BOOTLOADER_STATE  (rx)  : ORIGIN = 0x08000000, LENGTH = 16K
    RAM               (xrw) : ORIGIN = 0x20000000, LENGTH = 128K
}

__flash_base = 0x08000000;
__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - __flash_base;
",
        )
        .expect("записать memory.x");

        let regions = super::parse_memory_regions(&path).expect("раскладка обязана разбираться");

        let names = regions
            .iter()
            .map(|(name, _)| name.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            names,
            ["FLASH", "BOOTLOADER_STATE", "RAM"],
            "лишние строки в разборе"
        );
        let (name, flash) = &regions[0];
        assert_eq!(name, "FLASH");
        assert_eq!((flash.origin, flash.length), (0x0802_0000, 128 * 1024));
    }

    /// Комментарий после значения — не ошибка, а избыточная форма записи, которую
    /// линкер понимает и на которой человек вписывает пояснение.
    #[test]
    fn accepts_a_comment_after_the_length() {
        let dir = scratch("comment");
        let path = dir.path().join("memory.x");
        fs::write(
            &path,
            "MEMORY {\n    FLASH (rx) : ORIGIN = 0x08000000, LENGTH = 64K /* раздел под приложение */\n}\n",
        )
        .expect("записать memory.x");

        let regions = super::parse_memory_regions(&path).expect("комментарий не должен мешать");

        assert_eq!(regions.len(), 1);
        assert_eq!(regions[0].1.length, 64 * 1024);
    }

    /// Формы, которые линкер принимает, а парсер не знает. Раньше такой регион
    /// просто исчезал: `build` подписывал образ, не сверив ни базу, ни размер, и
    /// ошибка выглядела бы не как поломка, а как «всё в порядке».
    #[test]
    fn skips_regions_commented_out_by_the_generator() {
        let dir = scratch("commented-region");
        let path = dir.path().join("memory.x");
        fs::write(
            &path,
            "MEMORY {
    FLASH (rx) : ORIGIN = 0x08000000, LENGTH = 64K
                 /* FMC (rw) : ORIGIN = 0x60000000, LENGTH = 1K - внешняя шина */
}
",
        )
        .expect("записать memory.x");
        let regions = super::parse_memory_regions(&path).expect("комментарий не регион");
        assert_eq!(regions.len(), 1);
    }

    #[test]
    fn refuses_forms_it_does_not_know() {
        let dir = scratch("unknown-form");
        let path = dir.path().join("memory.x");

        // Регион, разбитый на две строки: половина условия не разбирается вовсе.
        fs::write(
            &path,
            "MEMORY {\n    FLASH (rx) : ORIGIN = 0x08000000,\n            LENGTH = 64K\n}\n",
        )
        .expect("записать memory.x");
        let Err(split) = super::parse_memory_regions(&path) else {
            panic!("разбитая на две строки строка обязана быть ошибкой");
        };
        assert!(
            split.to_string().contains("не удалось разобрать"),
            "{split:#}"
        );

        // Функциональная форма `ORIGIN(SYMBOL)`: числа там нет, и подставить
        // вместо него что-либо молча нельзя.
        fs::write(
            &path,
            "MEMORY {\n    FLASH (rx) : ORIGIN(ACTIVE_START), LENGTH = 64K\n}\n",
        )
        .expect("записать memory.x");
        let Err(functional) = super::parse_memory_regions(&path) else {
            panic!("ORIGIN(SYMBOL) обязана быть ошибкой, а не молчаливым нулём");
        };
        let text = format!("{functional:#}");
        assert!(
            text.contains("ACTIVE_START"),
            "в сообщении нет самой строки: {text}"
        );
        assert!(text.contains("128K"), "нет образца формы: {text}");
    }

    /// Незаполненный шаблон `crates-cross/app/memory.x`: значения стоят внутри
    /// `/* … */`, и линкер такой файл не собирает. Раньше он разбирался в пустой
    /// список, и проект оставался без единой проверки.
    #[test]
    fn refuses_an_unfilled_template() {
        let dir = scratch("unfilled");
        let path = dir.path().join("memory.x");
        fs::write(
            &path,
            "MEMORY {\n    FLASH             (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */\n    \
             RAM               (xrw) : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */\n}\n",
        )
        .expect("записать memory.x");

        let Err(err) = super::parse_memory_regions(&path) else {
            panic!("незаполненный шаблон обязан быть ошибкой");
        };

        assert!(err.to_string().contains("не заполнена"), "{err:#}");
    }
}

#[cfg(test)]
pub(crate) mod scratch {
    //! Временные каталоги для тестов, которые пишут файлы.

    use std::{fs, path::PathBuf};

    /// Свой каталог на каждый тест — и на каждый ПРОЦЕСС.
    ///
    /// Имя включает pid не для красоты: `std::env::temp_dir()` общий на машину,
    /// а тесты пишут в него файлы ключей и `memory.x`. Два прогона разом (свой `cargo xtask
    /// test host` рядом с `template-check`, который зовёт то же самое в каждом
    /// сгенерированном проекте, или просто два клона репозитория) чистили бы
    /// каталог друг у друга посреди работы. На общем CI-раннере каталог с
    /// фиксированным именем к тому же мог бы принадлежать другому пользователю.
    ///
    /// Возвращается страж: он удаляет каталог при выходе из теста, в том числе
    /// при панике. Иначе в `%TEMP%` оставался бы настоящий закрытый ключ
    /// ed25519 — пусть и от игрушечного проекта, привычка плохая.
    pub struct Scratch(PathBuf);

    impl Drop for Scratch {
        fn drop(&mut self) {
            if self.0.exists() {
                // В Drop разбирать ошибку некому: тест уже закончился, а
                // паниковать в Drop нельзя — при развёртывании стека это
                // прервало бы процесс.
                drop(fs::remove_dir_all(&self.0));
            }
        }
    }

    impl Scratch {
        pub fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    pub fn scratch(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("xtask-key-{name}-{}", std::process::id()));
        if dir.exists() {
            fs::remove_dir_all(&dir).expect("очистить каталог прошлого прогона");
        }
        fs::create_dir_all(&dir).expect("создать временный каталог");
        Scratch(dir)
    }
}
