//! Проверка шаблона целиком: сгенерировать проект и убедиться, что он
//! собирается — то, что до сих пор делалось руками по инструкции из CLAUDE.md
//! (найти все `{{...}}`/`{% ... %}`, подставить значения, выбрать ветку
//! Liquid-условия, собрать, откатить правки, не забыть проверить `git diff`).
//!
//! Запуск (крейт не член ни одного workspace — только через `--manifest-path`):
//!
//! ```text
//! cargo run --manifest-path chip-data-gen/Cargo.toml --bin template-check
//! cargo run --manifest-path chip-data-gen/Cargo.toml --bin template-check -- --quick
//! cargo run --manifest-path chip-data-gen/Cargo.toml --bin template-check -- stm32g071rb
//! ```
//!
//! Ловит ровно тот класс поломок, который не виден ни линтом, ни тестами
//! репозитория шаблона: у самого шаблона `Cargo.lock` пинит ревизии
//! git-зависимостей, поэтому ломающее изменение в `rust-lib` не видно нигде,
//! пока кто-нибудь не обновит зависимости. Здесь это делается после первой
//! генерации полного прогона — так и всплыла в своё время исчезнувшая у `fsm`
//! фича `macro`. Зависимости `crates-cross` (`supervisor`, `watchdog`) в этом
//! не нуждаются: их lock шаблон не раздаёт, и каждая конфигурация резолвит их
//! заново сама.
//!
//! Конфигурации по умолчанию подобраны так, чтобы задеть все ветки
//! генерации: обычный одноядерный, чип с выбором банковой схемы, чип, куда
//! OTA не помещается, тот же обычный чип с OTA, отключённой при генерации,
//! двухъядерный (`init_primary` вместо `init`), чип с 2 KiB RAM на
//! Cortex-M0+, раздел настроек на мелких секторах (влезает вместе с OTA) и на
//! крупных (вытесняет её), подпись образа и чип, у которого стёртый флеш
//! читается нулями. Итого десять — точный список в `default_cases()`. Свой
//! набор чипов — позиционными аргументами.

use std::{
    collections::BTreeSet,
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

/// Каждая запись здесь включает свою ветку генерации — см. doc-комментарий
/// выше. Менять только вместе с пониманием, какую ветку теряем.
///
/// Второе поле — суффикс имени каталога: он нужен там, где один и тот же чип
/// проверяется в разных конфигурациях, иначе прогоны затирали бы друг друга.
/// Третье — дополнительные `--define`.
fn default_cases() -> Vec<Case> {
    vec![
        // Одноядерный с OTA, одна карта памяти — самый обычный случай.
        Case::new("stm32f407ve"),
        // Тот же чип, но от OTA отказались при генерации: ветка «один образ на
        // весь flash» на чипе, куда схема прекрасно помещается.
        Case::new("stm32f407ve").variant("no-ota", &["ota=no"]),
        // Несколько карт памяти: в crates-cross/Cargo.toml должна появиться фича
        // "single-bank", без неё build.rs embassy-stm32 паникует.
        Case::new("stm32f429zg"),
        // OTA не помещается: crates-cross/boot удаляется из проекта, members и xtask
        // должны это пережить.
        Case::new("stm32h723ve"),
        // Двухъядерный: bsp/boot получают init_primary() с SharedData.
        //
        // Ядро именно CM4, а не CM7: у двухъядерных H7 блок сторожа зовётся
        // `IWDG2` у CM4 и `IWDG1` у CM7 (см. таблицу WATCHDOG в
        // chip-select.rhai), и `IWDG2` больше не покрыт ничем другим —
        // `IWDG1` берёт на себя одноядерный `stm32h723ve` выше. До появления
        // этой таблицы обе половины давали одинаковый вывод, и выбор ядра
        // здесь ничего не решал.
        Case::new("stm32h745zi-cm4"),
        // Другой конец линейки: 2 KiB RAM и Cortex-M0+ (thumbv6m, без CAS в
        // железе). Резерв под PERSIST/PANIC здесь считается долей RAM, а не
        // фиксированным килобайтом — на таком чипе тот был бы половиной памяти.
        Case::new("stm32l011f4"),
        // Раздел настроек вместе с OTA: мелкие секторы (2 KiB), раздел стоит
        // 4 KiB, схема OTA после него всё ещё помещается — в проект попадают
        // разом bootloader, ota.rs и config.rs.
        Case::new("stm32g071rb").variant("config", &["config=yes"]),
        // Он же, но там, где раздел вытесняет OTA: у F407 сектор 128 KiB,
        // раздел стоит 256 KiB из 512 KiB, и на ACTIVE+DFU уже не хватает.
        // Ветка «config есть, bootloader и ota.rs удалены» — только здесь.
        Case::new("stm32f407ve").variant("config", &["config=yes"]),
        // Подпись образа: фича `ed25519-salty` у embassy-boot, `salty` со
        // своей `slow-motion` и `verify_and_mark_updated` вместо
        // `mark_updated` в bsp. Проверяется на чипе с OTA — без него подпись
        // хук выключает.
        Case::new("stm32f407ve").variant("signed", &["signed=yes"]),
        // Единственная ветка, где стёртый флеш читается нулями: у L0/L1 хук
        // включает `flash-erase-zero` у embassy-boot, иначе bootloader читает
        // признак «стёрто» наоборот. Взят чип с запасом flash — на 64 KiB
        // раздел ACTIVE выходит меньше минимальной debug-прошивки (см. там же
        // известные ограничения).
        Case::new("stm32l073rz"),
    ]
}

/// Одна проверяемая конфигурация: чип плюс, если нужно, дополнительные
/// `--define` и суффикс, отличающий её каталог от других прогонов того же чипа.
struct Case {
    chip: String,
    suffix: Option<String>,
    defines: Vec<String>,
}

impl Case {
    fn new(chip: &str) -> Self {
        Self {
            chip: chip.to_owned(),
            suffix: None,
            defines: Vec::new(),
        }
    }

    fn variant(self, suffix: &str, defines: &[&str]) -> Self {
        Self {
            suffix: Some(suffix.to_owned()),
            defines: defines.iter().map(|d| (*d).to_owned()).collect(),
            ..self
        }
    }

    /// Имя проекта, оно же имя каталога.
    fn name(&self) -> String {
        match &self.suffix {
            // Дефис в чип-фиче двухъядерного чипа (`stm32h745zi-cm7`)
            // переносим как есть: cargo-generate санитизирует имя в
            // kebab-case и молча переименовывает каталог, так что `_` привёл
            // бы к поиску несуществующего пути.
            Some(suffix) => format!("tc-{}-{suffix}", self.chip),
            None => format!("tc-{}", self.chip),
        }
    }

    fn label(&self) -> String {
        match &self.suffix {
            Some(suffix) => format!("{} ({suffix})", self.chip),
            None => self.chip.clone(),
        }
    }
}

/// Файлы, где сырые `{{...}}` остаются намеренно и после генерации.
/// `CLAUDE.md` документирует плейсхолдеры как текст и потому исключён из
/// Liquid-подстановки (`exclude` в cargo-generate.toml).
const RAW_PLACEHOLDERS_ALLOWED: &[&str] = &["CLAUDE.md"];

/// Каталоги, которые не обходим при поиске остаточных плейсхолдеров.
const SKIP_DIRS: &[&str] = &[".git", "target"];

/// Что не нужно копии шаблона, из которой идёт генерация: каталоги сборки (на
/// любом уровне — их два, в корне и в `crates-cross/`), история и сам этот
/// инструмент, у которого свой `target/` рядом с исходниками.
const SKIP_IN_TEMPLATE: &[&str] = &[".git", "target", "chip-data-gen"];

struct Options {
    cases: Vec<Case>,
    ci: String,
    /// Только сгенерировать и проверить рендер, без сборки — быстрая проверка
    /// хуков и Liquid-условий (секунды вместо минут на чип).
    quick: bool,
    /// Не удалять сгенерированные проекты после успеха.
    keep: bool,
}

fn main() -> anyhow::Result<()> {
    let options = parse_args()?;
    let repo_root = repo_root();

    // Вне репозитория, и это важнее удобства: `cargo generate --path` копирует
    // каталог шаблона ЦЕЛИКОМ во временную папку, прежде чем прочитать
    // cargo-generate.toml, так что `ignore = ["target", ...]` от копирования
    // не спасает — он решает лишь, что попадёт в готовый проект. Кеш сборки
    // здесь общий на все конфигурации (второй прогон не пересобирает то, что
    // от чипа не зависит) и за десяток прогонов дорастает до гигабайтов;
    // внутри `target/` он превращал каждую генерацию из локального пути в
    // копирование этих гигабайтов. Однажды это кончилось «Недостаточно места
    // на диске (os error 112)» на ровном месте.
    let work_dir = env::temp_dir().join("rust-embedded-template-check");
    fs::create_dir_all(&work_dir)
        .with_context(|| format!("создать рабочий каталог {}", work_dir.display()))?;
    let cargo_target_dir = work_dir.join("cargo-target");

    // Генерируем не из самого репозитория, а из его облегчённой копии — по той
    // же причине, что и work_dir выше: `--path` копирует каталог целиком, и
    // `target/` (гигабайты мелких файлов) уходит в копию на каждую
    // конфигурацию. Замерено: генерация из репозитория с 3 ГБ в `target/` —
    // 169 секунд, из копии без него — секунда. Девять конфигураций превращали
    // это в семнадцать минут ожидания на ровном месте.
    let template = work_dir.join("template");
    copy_template(&repo_root, &template)
        .with_context(|| format!("скопировать шаблон в {}", template.display()))?;

    let labels: Vec<String> = options.cases.iter().map(Case::label).collect();
    println!(
        "Проверяем шаблон на {} конфигурациях (ci={}{}): {}",
        options.cases.len(),
        options.ci,
        if options.quick { ", --quick" } else { "" },
        labels.join(", "),
    );

    for (index, case) in options.cases.iter().enumerate() {
        // Обновление зависимостей и сверку lock делаем только на первой
        // конфигурации: граф пакетов от чипа не зависит (чипы отличаются
        // фичами embassy-stm32, а не составом), так что ломающее изменение в
        // rust-lib вылезет и с одного прогона, а девять подряд стоили бы минут
        // пять на пустом месте. Сверка там же и по второй причине: в
        // конфигурации без OTA cross-lock короче на boot и embassy-boot —
        // это не расхождение, а другая раскладка проекта.
        // `--quick` не обновляет зависимости вовсе, и это не экономия времени:
        // `cargo update` лезет в git за `rust-lib`, а он приватный. CI самого
        // шаблона доступа туда не имеет — с обновлением он падал бы на первой
        // же конфигурации, хотя проверять там надо только генерацию.
        let refresh = index == 0 && !options.quick;
        check_one(
            &template,
            &repo_root,
            &work_dir,
            &cargo_target_dir,
            case,
            &options,
            refresh,
        )?;
    }

    println!(
        "\nГотово: {} конфигураций прошли {}.",
        options.cases.len(),
        if options.quick {
            "генерацию"
        } else {
            "генерацию, lint и сборку"
        },
    );
    Ok(())
}

/// `template` — облегчённая копия, из которой генерируем; `repo_root` —
/// настоящий репозиторий, он нужен только чтобы сравнить lock-файлы и назвать
/// в подсказке путь, который правят руками.
fn check_one(
    template: &Path,
    repo_root: &Path,
    work_dir: &Path,
    cargo_target_dir: &Path,
    case: &Case,
    options: &Options,
    refresh: bool,
) -> anyhow::Result<()> {
    // Имя проекта = имя каталога, в который cargo-generate его положит.
    let name = case.name();
    let project = work_dir.join(&name);
    if project.exists() {
        fs::remove_dir_all(&project)
            .with_context(|| format!("очистить прошлый прогон {}", project.display()))?;
    }

    println!("\n=== {} ===", case.label());
    let mut command = Command::new("cargo");
    command
        .arg("generate")
        .arg("--path")
        .arg(template)
        .arg("--name")
        .arg(&name)
        .arg("--define")
        .arg(format!("chip_feature={}", case.chip))
        .arg("--define")
        .arg(format!("ci={}", options.ci));
    for define in &case.defines {
        command.arg("--define").arg(define);
    }
    run(
        command.arg("--silent").arg("--destination").arg(work_dir),
        work_dir,
        None,
    )
    .with_context(|| format!("генерация проекта под {}", case.label()))?;

    check_no_raw_placeholders(&project)?;

    // Обновление зависимостей — здесь, а не в post-хуке шаблона: пользователю
    // оно ни к чему (только замедляет генерацию и уводит проект с проверенных
    // версий), а вот проверке нужно. Ломающие изменения незапиненных
    // git-зависимостей (`rust-lib`) видны только так: у самого шаблона
    // Cargo.lock пинит ревизии, и без этого шага все проверки остаются
    // зелёными даже когда `main` библиотеки уже несовместим.
    //
    // `crates-cross` в списке нет, и это не пропуск: шаблон не раздаёт его
    // lock, так что сборка любой конфигурации и так резолвит `supervisor` с
    // `watchdog` заново. Обновлять нечего — `cargo update` там лишь сделал бы
    // раньше ровно то, что через минуту делает `cargo xtask build`.
    for manifest in if refresh {
        [None, Some("crates-host/chip-info/Cargo.toml")].as_slice()
    } else {
        &[]
    } {
        let mut update = Command::new("cargo");
        update.arg("update");
        if let Some(path) = manifest {
            update.arg("--manifest-path").arg(path);
        }
        run(&mut update, &project, None).with_context(|| {
            format!(
                "`cargo update` {} в сгенерированном проекте — свежие версии зависимостей \
                 не резолвятся",
                manifest.unwrap_or("(корень)"),
            )
        })?;
    }

    // Только там, где lock-файлы только что пересобраны: без `cargo update`
    // они побайтово те же, что в шаблоне, и сравнение не может ничего найти.
    if refresh {
        report_lock_drift(repo_root, &project);
    }

    if !options.quick {
        // Ровно те команды, которые README обещает пользователю шаблона.
        //
        // `xtask pins` — единственный способ вообще проверить `chip-info`:
        // его манифест до генерации не резолвится (Liquid в чип-фиче), так что
        // ни `cargo xtask lint`, ни CI самого шаблона его не трогают. Оба
        // вызова не случайны: без аргумента идёт перечисление блоков, с `RCC` —
        // ветка с таблицей выводов, DMA и тактированием, а `RCC` есть на любом
        // STM32 (в отличие от, например, `SPI4`).
        //
        // fmt/clippy для chip-info — здесь же и по той же причине; в
        // сгенерированном проекте манифест уже подставлен, и обе команды
        // работают. `cargo xtask lint` их намеренно не зовёт: тогда каждый
        // пользовательский CI тянул бы stm32-metapac ради инструмента, который
        // на сборку прошивки никак не влияет.
        let commands: [&[&str]; 10] = [
            &["xtask", "lint"],
            &["xtask", "test", "host"],
            &["xtask", "lint", "cross"],
            &["xtask", "build"],
            &["xtask", "pins"],
            &["xtask", "pins", "RCC"],
            // Заготовка — на I2C1, и это не произвольный выбор: он есть на
            // всех чипах набора (у stm32l011f4, например, USART1 нет вовсе —
            // только USART2 и LPUART1), а обработчиков прерывания у него два
            // и они разные, то есть проверяется самая ошибкоопасная ветка
            // таблицы. `--check` на нетронутом resources.rs обязан проходить:
            // шаблон не занимает ни одного вывода.
            &["xtask", "pins", "I2C1", "--snippet"],
            &["xtask", "pins", "--check"],
            &[
                "fmt",
                "--manifest-path",
                "crates-host/chip-info/Cargo.toml",
                "--check",
            ],
            &[
                "clippy",
                "--manifest-path",
                "crates-host/chip-info/Cargo.toml",
                "--all-targets",
                "--",
                "-D",
                "warnings",
            ],
        ];
        check_snippets_compile(&project, cargo_target_dir)?;

        for args in commands {
            run(
                Command::new("cargo").args(args),
                &project,
                Some(cargo_target_dir),
            )
            .with_context(|| {
                format!(
                    "`cargo {}` в сгенерированном проекте (оставлен для разбора: {})",
                    args.join(" "),
                    project.display(),
                )
            })?;
        }
    }

    if options.keep {
        println!("проект оставлен: {}", project.display());
    } else {
        fs::remove_dir_all(&project).with_context(|| format!("удалить {}", project.display()))?;
    }
    Ok(())
}

/// Сравнивает списки пакетов в lock-файлах шаблона и свежесгенерированного
/// проекта — их там только что пересобрал `cargo update`. Зовётся только под
/// `refresh` (первая конфигурация полного прогона): без обновления файлы
/// побайтово те же, что в шаблоне, и сравнивать было бы нечего.
///
/// Расхождение почти всегда значит одно: манифест правили, а lock не
/// перегенерировали. Само по себе это тихо: в репозитории шаблона `chip-info`
/// никто не собирает, а вот CI сгенерированного проекта зовёт `cargo run
/// --locked` и на отставшем lock падает. Чинить — по рецепту из «Lock-файлы
/// шаблона» в MAINTAINING.md. Предупреждение, а не ошибка: часть расхождений —
/// обычные обновления версий из crates.io, за которые шаблон не отвечает.
///
/// Оговорка, которую эта проверка НЕ ловит: набор пакетов может совпасть, а
/// lock всё равно устареть — так бывает, когда зависимость переезжает между
/// членами воркспейса. Проверено на живом примере (`watchdog` из `app` в
/// `bsp`): списки пакетов идентичны, а `cargo fetch --locked` падает.
///
/// `crates-cross/Cargo.lock` в списке нет намеренно: шаблон его не раздаёт
/// (состав пакетов там зависит от ответов при генерации, см. «Lock-файлы
/// шаблона» в MAINTAINING.md), сравнивать не с чем — проект считает его сам
/// при первой сборке.
fn report_lock_drift(repo_root: &Path, project: &Path) {
    let locks = ["Cargo.lock", "crates-host/chip-info/Cargo.lock"];
    for lock in locks {
        let (Some(template), Some(generated)) = (
            lock_package_names(&repo_root.join(lock)),
            lock_package_names(&project.join(lock)),
        ) else {
            continue;
        };
        let missing: Vec<&String> = generated.difference(&template).collect();
        let extra: Vec<&String> = template.difference(&generated).collect();
        if !missing.is_empty() || !extra.is_empty() {
            println!("ВНИМАНИЕ: {lock} шаблона разошёлся с тем, что собралось при генерации");
            if !missing.is_empty() {
                println!("  нет в шаблонном lock: {missing:?}");
            }
            if !extra.is_empty() {
                println!("  лишние в шаблонном lock: {extra:?}");
            }
            println!("  поправить: скопировать {lock} из {}", project.display());
        }
    }
}

/// Пары `имя версия` из lock-файла. Версия входит в ключ намеренно: поднятая
/// в манифесте версия при неперегенерированном lock — ровно тот случай, на
/// котором падает `cargo fetch --locked` в CI, а по одним именам он выглядел
/// бы как совпадение.
fn lock_package_names(lock: &Path) -> Option<BTreeSet<String>> {
    let text = fs::read_to_string(lock).ok()?;
    let mut packages = BTreeSet::new();
    let mut name: Option<&str> = None;
    for line in text.lines() {
        if let Some(rest) = line.strip_prefix("name = \"") {
            name = rest.strip_suffix('"');
        } else if let Some(rest) = line.strip_prefix("version = \"")
            && let (Some(name), Some(version)) = (name.take(), rest.strip_suffix('"'))
        {
            packages.insert(format!("{name} {version}"));
        }
    }
    Some(packages)
}

/// Ищет `{{ }}`/`{% %}`, пережившие генерацию. Мимо линта и сборки такое
/// проходит незамеченным везде, кроме `.rs`/`.toml`: `.json`, `.yml` и
/// `memory.x` никто не компилирует, и сломанный `launch.json` обнаружился бы
/// только у пользователя.
fn check_no_raw_placeholders(project: &Path) -> anyhow::Result<()> {
    let mut found = Vec::new();
    visit_files(project, &mut |path| {
        let relative = path.strip_prefix(project).unwrap_or(path);
        let relative_str = relative.to_string_lossy().replace('\\', "/");
        if RAW_PLACEHOLDERS_ALLOWED.contains(&relative_str.as_str()) {
            return Ok(());
        }
        let text = match fs::read(path) {
            Ok(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
            Err(err) => bail!("прочитать {}: {err}", path.display()),
        };
        for (number, line) in text.lines().enumerate() {
            let line_without_actions = without_actions_expressions(line);
            if line_without_actions.contains("{{") || line_without_actions.contains("{%") {
                found.push(format!("{relative_str}:{}: {}", number + 1, line.trim()));
            }
        }
        Ok(())
    })?;

    if !found.is_empty() {
        bail!(
            "в сгенерированном проекте остались неподставленные плейсхолдеры:\n  {}",
            found.join("\n  "),
        );
    }
    println!("плейсхолдеры: не осталось ни одного");
    Ok(())
}

/// Вырезает выражения GitHub Actions (`${{ secrets.X }}`) — в сгенерированном
/// проекте это рабочий синтаксис, а не переживший генерацию плейсхолдер.
///
/// В шаблоне они обёрнуты в `{% raw %}`, иначе cargo-generate спотыкается о
/// неизвестную переменную и пропускает подстановку ВО ВСЁМ файле (проверено
/// эмпирически: рядом стоявший `{{target}}` тоже оставался сырым). Поэтому
/// послабление ничего не ослабляет: пропусти генерация такой файл целиком —
/// настоящие плейсхолдеры в нём никуда не денутся и будут найдены этой же
/// проверкой.
///
/// Незакрытое `${{` считается вырезанным до конца строки: выражения Actions
/// однострочные, а ложное срабатывание здесь дороже пропуска.
fn without_actions_expressions(line: &str) -> String {
    let mut rest = line;
    let mut out = String::new();
    while let Some(start) = rest.find("${{") {
        out.push_str(&rest[..start]);
        let after = &rest[start + "${{".len()..];
        match after.find("}}") {
            Some(end) => rest = &after[end + "}}".len()..],
            None => rest = "",
        }
    }
    out.push_str(rest);
    out
}

fn visit_files(
    dir: &Path,
    visit: &mut impl FnMut(&Path) -> anyhow::Result<()>,
) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir).with_context(|| format!("прочитать {}", dir.display()))?
    {
        let entry = entry?;
        let path = entry.path();
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            let name = entry.file_name();
            if SKIP_DIRS.contains(&name.to_string_lossy().as_ref()) {
                continue;
            }
            visit_files(&path, visit)?;
        } else if file_type.is_file() {
            visit(&path)?;
        }
    }
    Ok(())
}

/// То же, что [`run`], но отдаёт stdout: нужно там, где проверяется не код
/// возврата, а напечатанное.
fn capture(
    command: &mut Command,
    current_dir: &Path,
    cargo_target_dir: Option<&Path>,
) -> anyhow::Result<String> {
    command.current_dir(current_dir);
    if let Some(target_dir) = cargo_target_dir {
        command.env("CARGO_TARGET_DIR", target_dir);
    }
    let output = command
        .output()
        .with_context(|| format!("запустить {:?}", command.get_program()))?;
    if !output.status.success() {
        bail!(
            "команда завершилась с {}:\n{}",
            output.status,
            String::from_utf8_lossy(&output.stderr),
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// Заготовки из `cargo xtask pins <БЛОК> --snippet` для ВСЕХ блоков чипа —
/// собираются в один временный модуль `bsp` и компилируются.
///
/// Зачем это здесь. Заготовку человек вставляет в свой проект как есть, то
/// есть она обязана компилироваться, — а проверялось до сих пор лишь то, что
/// команда не упала. Цена одного этого пробела: четыре дефекта разом, и
/// каждый ронял вставленный код у пользователя. Дублирующиеся ключи там, где
/// два сигнала делят вектор (`error[E0428]` на 217 чипах); имя структуры
/// `Usb_otg_fsResources`, которое не проходит `non_camel_case_types` под
/// `-D warnings`; несуществующий `adc::InterruptHandler` на большинстве
/// версий ADC; пустая `bind_interrupts!` для классического USB. Ни один из
/// них компилятор здесь не видел, потому что напечатанное никуда не
/// компилировалось.
///
/// Проверяются все блоки с выводами (у STM32F407 их 38), а не выборка: обе
/// половины заготовки зависят от блока, и «на I2C1 сходится» ничего не
/// говорит про USB или ADC.
///
/// Каждая заготовка — в своём модуле: `bind_interrupts!` объявляет `struct
/// Irqs`, и без модулей второй же блок дал бы конфликт имён. Линковки нет,
/// поэтому одинаковые `#[unsafe(no_mangle)]`-обработчики у блоков, делящих
/// вектор, друг другу не мешают: `clippy` только проверяет.
fn check_snippets_compile(project: &Path, cargo_target_dir: &Path) -> anyhow::Result<()> {
    let listing = capture(
        Command::new("cargo").args(["xtask", "pins"]),
        project,
        Some(cargo_target_dir),
    )?;
    let blocks = blocks_with_pins(&listing);
    anyhow::ensure!(
        !blocks.is_empty(),
        "`cargo xtask pins` не перечислил ни одного блока с выводами — разбор списка разошёлся с \
         форматом вывода chip-info",
    );

    // Половина `assign_resources!` берётся ровно у одного блока, и это не
    // экономия: макрос объявляет `split_resources` через `#[macro_export]`, то
    // есть кладёт его в корень крейта — двух вызовов в одном крейте не бывает
    // (`error[E0428]: the name split_resources is defined multiple times`).
    // Блок выбирается с подчёркиванием в имени, если такой есть: именно на нём
    // ловится имя структуры ресурсов (`USB_OTG_FS` давал
    // `Usb_otg_fsResources`, который не проходит `non_camel_case_types`).
    let with_resources = blocks
        .iter()
        .find(|block| block.contains('_'))
        .unwrap_or(&blocks[0])
        .clone();

    let mut module = String::from(
        "//! Временный модуль: сюда `template-check` складывает заготовки из\n\
         //! `cargo xtask pins <БЛОК> --snippet` и компилирует их. В проекте\n\
         //! пользователя этого файла нет — он создаётся и удаляется проверкой.\n\
         #![allow(dead_code)]\n\n",
    );
    for block in &blocks {
        let snippet = capture(
            Command::new("cargo").args(["xtask", "pins", block, "--snippet"]),
            project,
            Some(cargo_target_dir),
        )?;
        let bind = part_of(&snippet, "bind_interrupts!");
        if !bind.is_empty() {
            // Импортов модулю не добавляется ни одного: заготовка печатает
            // свои, и в этом половина смысла проверки — вставленная как есть,
            // она обязана собираться сама по себе.
            module.push_str(&format!(
                "mod irqs_{} {{\n{}\n}}\n\n",
                block.to_lowercase(),
                indent(&bind),
            ));
        }
        if *block == with_resources {
            let resources = part_of(&snippet, "assign_resources!");
            anyhow::ensure!(
                !resources.is_empty(),
                "в заготовке для {block} нет половины assign_resources! — разбор разошёлся с \
                 форматом вывода chip-info",
            );
            module.push_str(&format!("mod resources {{\n{}\n}}\n\n", indent(&resources)));
        }
    }

    let bsp = project.join("crates-cross").join("bsp").join("src");
    let module_path = bsp.join("snippet_check.rs");
    let lib_path = bsp.join("lib.rs");
    let lib = fs::read_to_string(&lib_path)?;
    fs::write(&module_path, &module)?;
    fs::write(&lib_path, format!("{lib}\nmod snippet_check;\n"))?;

    let checked = run(
        Command::new("cargo").args(["clippy", "-p", "bsp", "--", "-D", "warnings"]),
        &project.join("crates-cross"),
        Some(cargo_target_dir),
    );

    // Проект возвращается в исходное состояние в любом случае: следом за этой
    // проверкой идут другие, и им нужен нетронутый `bsp`.
    //
    // Результат восстановления НЕ пробрасывается здесь же: сорвись оно (файл
    // занят, права), сообщение об этом вытеснило бы настоящую причину —
    // «заготовка не компилируется», — ради которой прогон и затевался.
    // Сначала главное, потом уборка.
    let restored = fs::write(&lib_path, lib).and_then(|()| fs::remove_file(&module_path));

    checked.context(
        "заготовка из `cargo xtask pins --snippet` не компилируется — её вставляют в проект как \
         есть, значит чинить надо таблицу HANDLERS или печать в crates-host/chip-info",
    )?;
    restored.with_context(|| {
        format!(
            "заготовки компилируются, но временный модуль не убран — уберите вручную: {}",
            module_path.display(),
        )
    })?;
    println!("заготовки: {} блоков компилируются", blocks.len());
    Ok(())
}

/// Имена блоков из шапки `cargo xtask pins` без аргументов.
///
/// Формат её вывода — «блоки с выводами (N):» и дальше строки с именами через
/// пробел, до пустой строки.
fn blocks_with_pins(listing: &str) -> Vec<String> {
    listing
        .lines()
        .skip_while(|line| !line.starts_with("блоки с выводами"))
        .skip(1)
        .take_while(|line| !line.trim().is_empty())
        .flat_map(str::split_whitespace)
        .map(str::to_owned)
        .collect()
}

/// Одна половина напечатанной заготовки: от строки, начинающейся с `head`, и
/// до конца её блока — то есть до строки, где закрывается верхний уровень
/// скобок.
///
/// Пустая строка означает, что половины нет вовсе: `bind_interrupts!` не
/// печатается блокам, у которых в embassy нет обработчика.
///
/// Комментарии в заготовке адресованы человеку («вставьте это в main.rs») и в
/// модуль не нужны.
fn part_of(snippet: &str, head: &str) -> String {
    let mut lines: Vec<&str> = Vec::new();
    // Импорты половины печатаются перед её макросом, и без них она не
    // компилируется, — значит забирать надо вместе. Сбрасываются они на
    // комментарии-заголовке: `use` принадлежит той половине, за которой стоит.
    let mut imports: Vec<&str> = Vec::new();
    let mut depth = 0usize;
    let mut started = false;
    for line in snippet.lines() {
        if !started {
            if line.starts_with("// crates-cross/") {
                imports.clear();
            } else if line.starts_with("use ") {
                imports.push(line);
            }
            if !line.starts_with(head) {
                continue;
            }
            started = true;
            lines.append(&mut imports);
            lines.push("");
        }
        if line.trim_start().starts_with("//") {
            continue;
        }
        depth += line.matches('{').count();
        depth = depth.saturating_sub(line.matches('}').count());
        lines.push(line);
        if depth == 0 {
            break;
        }
    }
    lines.join("\n")
}

fn indent(code: &str) -> String {
    code.lines()
        .map(|line| {
            if line.trim().is_empty() {
                String::new()
            } else {
                format!("    {line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn run(
    command: &mut Command,
    current_dir: &Path,
    cargo_target_dir: Option<&Path>,
) -> anyhow::Result<()> {
    command.current_dir(current_dir);
    if let Some(target_dir) = cargo_target_dir {
        // Общий каталог сборки на все чипы: то, что от чипа не зависит
        // (domain и весь host-граф), собирается один раз на весь прогон.
        command.env("CARGO_TARGET_DIR", target_dir);
    }
    let status = command
        .status()
        .with_context(|| format!("запустить {:?}", command.get_program()))?;
    if !status.success() {
        bail!("команда завершилась с {status}");
    }
    Ok(())
}

fn parse_args() -> anyhow::Result<Options> {
    let mut cases: Vec<Case> = Vec::new();
    let mut ci = "github".to_owned();
    let mut quick = false;
    let mut keep = false;

    let mut args = env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--quick" => quick = true,
            "--keep" => keep = true,
            "--ci" => {
                ci = args
                    .next()
                    .context("--ci требует значения (github|gitlab|none)")?;
            }
            "-h" | "--help" => {
                println!(
                    "USAGE: template-check [--quick] [--keep] [--ci github|gitlab|none] \
                     [chip_feature ...]\n\n\
                     Без чипов проверяются: {}",
                    default_cases()
                        .iter()
                        .map(Case::label)
                        .collect::<Vec<_>>()
                        .join(", "),
                );
                std::process::exit(0);
            }
            other if other.starts_with('-') => bail!("неизвестный флаг: {other}"),
            other => cases.push(Case::new(other)),
        }
    }

    if cases.is_empty() {
        cases = default_cases();
    }
    Ok(Options {
        cases,
        ci,
        quick,
        keep,
    })
}

fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir
}

/// Копирует шаблон в отдельный каталог, пропуская то, что генерации не нужно:
/// `target/` (гигабайты артефактов), `.git/` и `chip-data-gen/` — сам этот
/// инструмент, у которого свой `target/` рядом с исходниками.
///
/// Прошлая копия сносится целиком: правки шаблона должны доезжать до проверки,
/// а не оставаться в устаревшем слепке.
fn copy_template(repo_root: &Path, dest: &Path) -> anyhow::Result<()> {
    if dest.exists() {
        fs::remove_dir_all(dest)
            .with_context(|| format!("очистить прошлую копию {}", dest.display()))?;
    }
    fs::create_dir_all(dest)?;

    for entry in fs::read_dir(repo_root)? {
        let entry = entry?;
        let name = entry.file_name();
        if SKIP_IN_TEMPLATE.contains(&name.to_string_lossy().as_ref()) {
            continue;
        }
        let from = entry.path();
        let to = dest.join(&name);
        if entry.file_type()?.is_dir() {
            copy_dir(&from, &to)?;
        } else {
            fs::copy(&from, &to)
                .with_context(|| format!("скопировать {} в {}", from.display(), to.display()))?;
        }
    }
    Ok(())
}

fn copy_dir(from: &Path, to: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(to)?;
    for entry in fs::read_dir(from)? {
        let entry = entry?;
        // Пропуск нужен на каждом уровне, а не только в корне: `crates-cross/target`
        // — такой же каталог сборки, и без этой проверки копия весила 2.2 ГБ
        // вместо десятка мегабайт, а генерация из неё занимала полминуты.
        if SKIP_IN_TEMPLATE.contains(&entry.file_name().to_string_lossy().as_ref()) {
            continue;
        }
        let source = entry.path();
        let destination = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            copy_dir(&source, &destination)?;
        } else {
            fs::copy(&source, &destination).with_context(|| {
                format!(
                    "скопировать {} в {}",
                    source.display(),
                    destination.display()
                )
            })?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::without_actions_expressions;

    /// Главное свойство: послабление ради Actions не должно прятать настоящий
    /// плейсхолдер, стоящий в той же строке.
    #[test]
    fn keeps_a_liquid_placeholder_next_to_an_actions_expression() {
        let line = "        run: gh release create {{project-name}} --token ${{ secrets.TOKEN }}";

        assert!(without_actions_expressions(line).contains("{{"));
    }

    #[test]
    fn strips_an_actions_expression() {
        let line = "          GH_TOKEN: ${{ secrets.GITHUB_TOKEN }}";

        let cleaned = without_actions_expressions(line);
        assert!(
            !cleaned.contains("{{"),
            "осталось выражение Actions: {cleaned}"
        );
    }

    /// Несколько выражений в строке — обычное дело для `if:`.
    #[test]
    fn strips_every_actions_expression_in_a_line() {
        let line = "    if: ${{ github.event_name == 'push' }} && ${{ success() }}";

        assert!(!without_actions_expressions(line).contains("{{"));
    }

    /// Незакрытое выражение не должно уводить функцию в бесконечный цикл или
    /// панику по границе среза — только этого и проверяем.
    #[test]
    fn survives_an_unclosed_actions_expression() {
        assert!(!without_actions_expressions("value: ${{ secrets.X").contains("{{"));
    }
}
