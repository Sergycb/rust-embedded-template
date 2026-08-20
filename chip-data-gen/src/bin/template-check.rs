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
//! репозитория шаблона: у самого шаблона `Cargo.lock` пинит git-зависимости,
//! а `post-script.rhai` при генерации делает `cargo update` — ломающее
//! изменение в `rust-lib` роняет генерацию, оставляя локальные проверки
//! зелёными (так и случилось с фичей `macro` у `fsm`).
//!
//! Конфигурации по умолчанию подобраны так, чтобы задеть все ветки
//! генерации: обычный одноядерный, чип с выбором банковой схемы, чип, куда
//! OTA не помещается, тот же обычный чип с OTA, отключённой при генерации,
//! двухъядерный (`init_primary` вместо `init`) и чип с 2 KiB RAM на
//! Cortex-M0+. Свой список чипов — позиционными аргументами.

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
        // Несколько карт памяти: в cross/Cargo.toml должна появиться фича
        // "single-bank", без неё build.rs embassy-stm32 паникует.
        Case::new("stm32f429zg"),
        // OTA не помещается: cross/boot удаляется из проекта, members и xtask
        // должны это пережить.
        Case::new("stm32h723ve"),
        // Двухъядерный: bsp/boot получают init_primary() с SharedData.
        Case::new("stm32h745zi-cm7"),
        // Другой конец линейки: 2 KiB RAM и Cortex-M0+ (thumbv6m, без CAS в
        // железе). Резерв под PERSIST/PANIC здесь считается долей RAM, а не
        // фиксированным килобайтом — на таком чипе тот был бы половиной памяти.
        Case::new("stm32l011f4"),
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

    let labels: Vec<String> = options.cases.iter().map(Case::label).collect();
    println!(
        "Проверяем шаблон на {} конфигурациях (ci={}{}): {}",
        options.cases.len(),
        options.ci,
        if options.quick { ", --quick" } else { "" },
        labels.join(", "),
    );

    for case in &options.cases {
        check_one(&repo_root, &work_dir, &cargo_target_dir, case, &options)?;
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

fn check_one(
    repo_root: &Path,
    work_dir: &Path,
    cargo_target_dir: &Path,
    case: &Case,
    options: &Options,
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
        .arg(repo_root)
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
        command
            .arg("--silent")
            // Без него post-script.rhai (cargo update) не выполнится в
            // неинтерактивном режиме — а именно он и ловит ломающие
            // изменения незапиненных git-зависимостей.
            .arg("--allow-commands")
            .arg("--destination")
            .arg(work_dir),
        work_dir,
        None,
    )
    .with_context(|| format!("генерация проекта под {}", case.label()))?;

    check_no_raw_placeholders(&project)?;
    report_lock_drift(repo_root, &project);

    if !options.quick {
        // Ровно те команды, которые README обещает пользователю шаблона.
        let commands: [&[&str]; 4] = [
            &["xtask", "lint"],
            &["xtask", "test", "host"],
            &["xtask", "lint", "cross"],
            &["xtask", "build"],
        ];
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
/// проекта (там их только что пересобрал `cargo update` из post-хука).
///
/// Расхождение почти всегда значит одно: манифест правили, а lock не
/// перегенерировали. В проекте пользователя это незаметно — `cargo update`
/// при генерации всё чинит, — но в самом репозитории шаблона `cross` никто не
/// собирает, а CI сгенерированного проекта зовёт `cargo fetch --locked` и на
/// отставшем lock падает. Предупреждение, а не ошибка: часть расхождений —
/// обычные обновления версий из crates.io, за которые шаблон не отвечает.
fn report_lock_drift(repo_root: &Path, project: &Path) {
    for lock in ["Cargo.lock", "cross/Cargo.lock"] {
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
            if line.contains("{{") || line.contains("{%") {
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
