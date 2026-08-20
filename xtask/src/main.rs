use std::{env, fs, path::Path, path::PathBuf};

use anyhow::Context;
use xshell::cmd;

const CHIP: &str = "{{chip}}";
/// Целевой triple прошивки — тот же, что в `cross/.cargo/config.toml`. Нужен,
/// чтобы `cargo xtask setup` поставил ровно тот target, под который собирается
/// проект.
const TARGET: &str = "{{target}}";

/// Профиль по умолчанию для `flash`: во время разработки прошивают чаще, чем
/// при выпуске.
const DEFAULT_PROFILE: &str = "debug";

fn main() -> Result<(), anyhow::Error> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let args = args.iter().map(|s| &**s).collect::<Vec<_>>();
    let sh = xshell::Shell::new()?;

    match &args[..] {
        ["setup"] => setup(&sh),
        // Без аргумента — `host`: единственный этап, которому не нужна плата.
        ["test"] | ["test", "host"] => test_host(&sh),
        ["test", "all"] => test_all(&sh),
        ["test", "host-target"] => test_host_target(&sh),
        ["test", "target"] => test_target(&sh),
        ["build"] => build(&sh),
        ["flash"] => flash_all(&sh, DEFAULT_PROFILE),
        ["flash", profile] => flash_all(&sh, profile),
        ["lint"] => lint_host(&sh),
        ["lint", "cross"] => lint_cross(&sh),
        // Без аргументов — просто справка, это не ошибка. А вот непонятая
        // команда завершается ненулевым кодом: иначе опечатка в CI-шаге или в
        // задаче IDE даёт зелёный прогон, в котором ничего не выполнилось.
        [] => {
            usage();
            Ok(())
        }
        other => {
            usage();
            anyhow::bail!("неизвестная команда: cargo xtask {}", other.join(" "))
        }
    }
}

fn usage() {
    println!("USAGE cargo xtask setup                      # target, probe-rs, flip-link, nextest");
    println!("      cargo xtask build                      # cross: debug + release");
    println!("      cargo xtask flash [debug|release]      # прошить bootloader + приложение");
    println!("      cargo xtask lint [cross]");
    println!("      cargo xtask test [host|target|host-target|all]");
    println!();
    println!("Всё остальное про плату — напрямую через probe-rs:");
    println!("      probe-rs attach --chip {CHIP} <elf>   # defmt-лог без перепрошивки");
    println!("      probe-rs reset  --chip {CHIP}");
    println!("      probe-rs erase  --chip {CHIP} --connect-under-reset");
}

/// Ставит всё, без чего `build`/`run` падают с невнятной ошибкой линкера или
/// `no such command`.
fn setup(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    // Компоненты — не «на всякий случай»: профиль rustup бывает minimal (так
    // устроены и оба CI-образа), и тогда `cargo xtask lint` сразу после setup
    // падал бы на `cargo fmt`/`cargo clippy` с "no such command".
    cmd!(sh, "rustup component add rustfmt clippy").run()?;
    cmd!(sh, "rustup target add {TARGET}").run()?;

    // Проверяем именно бинарники, а не `cargo <подкоманда>`: `cargo nextest
    // --version` на машине без nextest запускается успешно — это сам `cargo`
    // печатает «no such command», — и установка молча пропускалась бы.
    install_if_missing(sh, "probe-rs", "probe-rs-tools")?;
    install_if_missing(sh, "flip-link", "flip-link")?;
    install_if_missing(sh, "cargo-nextest", "cargo-nextest")?;
    Ok(())
}

/// Ставит пакет, только если его бинарника ещё нет.
///
/// Голый `cargo install` для этого не годится: пропускает он лишь ту же
/// версию, а при установленной постарше падает с «binary `probe-rs.exe`
/// already exists in destination, add --force» — то есть `cargo xtask setup`
/// начинает падать ровно у тех, у кого всё уже стоит. Ставить с `--force`
/// тоже не вариант: тогда каждый запуск пересобирает probe-rs-tools из
/// исходников. Обновляются инструменты отдельно и осознанно
/// (`cargo install <пакет> --force`), задача setup — довести пустую машину до
/// рабочего состояния.
///
/// Признак «установлен» — то, что процесс запустился, а не его код возврата:
/// `flip-link --version` без аргументов линкера отвечает справкой lld и
/// ненулевым кодом, хотя сам на месте. Отсутствующая программа даёт `Err`
/// ещё до запуска. По `cargo install --list` проверять нельзя: инструмент
/// могли поставить `cargo binstall`'ом или пакетным менеджером, и тогда его
/// в списке нет, а бинарник есть.
fn install_if_missing(
    sh: &xshell::Shell,
    binary: &str,
    package: &str,
) -> Result<(), anyhow::Error> {
    let installed = cmd!(sh, "{binary} --version")
        .quiet()
        .ignore_status()
        .output()
        .is_ok();
    if installed {
        println!("{package}: уже установлен, пропускаем");
        return Ok(());
    }
    cmd!(sh, "cargo install {package} --locked").run()?;
    Ok(())
}

fn flash_all(sh: &xshell::Shell, profile: &str) -> Result<(), anyhow::Error> {
    flash_boot(sh, profile)?;
    flash_app(sh, profile)?;
    Ok(())
}

fn build(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir().join("cross"));
    cmd!(sh, "cargo build").run()?;
    cmd!(sh, "cargo build --release").run()?;
    Ok(())
}

/// Whether this project has a bootloader at all — substituted at generation
/// time. `"false"` when the OTA layout does not fit the chip's flash: then
/// `cross/boot` is not part of the generated project (see `chip-select.rhai`),
/// and the app is flashed straight to the start of flash.
///
/// A string rather than a `bool` literal on purpose: the template source has
/// to stay parseable Rust (`xtask` is a member of the root workspace and is
/// linted there), and a template placeholder only survives that inside a
/// string literal.
const OTA: &str = "{{ota}}";

/// Compared against `"false"` rather than `"true"` so that the un-rendered
/// template (where `OTA` is still the literal placeholder) behaves like a
/// project *with* a bootloader — that is what the maintainer checking
/// `cargo xtask lint cross` in the template repo itself expects. Generated
/// projects always get an exact `"true"`/`"false"`.
fn has_bootloader() -> bool {
    OTA != "false"
}

fn test_all(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    test_host(sh)?;
    test_target(sh)?;
    test_host_target(sh)
}

fn test_host(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir());
    cmd!(
        sh,
        "cargo nextest run --workspace --exclude host-target-tests --release --features domain/log,domain/std"
    )
    .run()?;
    Ok(())
}

/// Хост управляет уже прошитым устройством: сначала заливаем то, что тест
/// будет проверять, потом запускаем сам тест. Чип и адрес региона `PERSIST`
/// уходят к нему через окружение: первое подставляется при генерации в одном
/// месте, второе считается по `memory.x` — знать это в двух местах незачем.
fn test_host_target(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    flash_boot(sh, "release")?;
    flash_app(sh, "release")?;

    let memory_x = root_dir().join("cross").join("app").join("memory.x");
    let regions = parse_memory_regions(&memory_x)
        .with_context(|| format!("разобрать {}", memory_x.display()))?;
    let persist = region(&regions, "PERSIST").context(
        "в cross/app/memory.x нет региона PERSIST — на этом чипе host-target тесту \
         не за что зацепиться",
    )?;

    {
        let _env_chip = sh.push_env("HOST_TARGET_CHIP", CHIP);
        let _env_persist =
            sh.push_env("HOST_TARGET_PERSIST_ADDR", format!("{:#x}", persist.origin));
        let _p = sh.push_dir(root_dir().join("host-target-tests"));
        cmd!(sh, "cargo nextest run").run()?;
    }
    Ok(())
}

/// Тесты внутри МК. Bootloader заливается первым по той же причине, что и в
/// `test_host_target`: тестовый образ линкуется в `ACTIVE` (его `memory.x` —
/// копия app'ового), а `probe-rs run` после заливки сбрасывает чип, и
/// управление получает не тест, а то, что лежит с базы flash. На плате, где
/// bootloader'а нет — свежей или после `probe-rs erase`, — без этой строки
/// не выполнился бы ни один тест.
fn test_target(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    flash_boot(sh, "debug")?;
    let _p = sh.push_dir(root_dir().join("cross/target-tests"));
    cmd!(sh, "cargo test").run()?;
    Ok(())
}

fn lint_host(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir());
    cmd!(sh, "cargo fmt --check").run()?;
    cmd!(
        sh,
        "cargo clippy --workspace --all-targets --features domain/std,domain/log -- -D warnings"
    )
    .run()?;
    Ok(())
}

fn lint_cross(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir().join("cross"));
    cmd!(sh, "cargo fmt --check").run()?;
    // Без `--all-targets`: он добавил бы `lib test` — цель, которую cargo
    // строит для unit-тестов внутри библиотеки. Ей нужен крейт `test`, а его
    // для `thumbv*-none-*` не существует (E0463), так что no_std-библиотека
    // вроде `bsp` на ней падает всегда, независимо от своего содержимого.
    cmd!(sh, "cargo clippy --workspace -- -D warnings").run()?;
    // Тесты на устройстве при этом линтятся: у них свой harness и свой
    // `#![no_main]`, крейт `test` им не нужен — но и в `--workspace` выше они
    // не попадают, потому что это отдельная тестовая цель.
    cmd!(
        sh,
        "cargo clippy -p target-tests --test test -- -D warnings"
    )
    .run()?;
    // Second pass, release only, scoped to app+boot: the release profile turns
    // `debug-assertions`/`overflow-checks` off, so anything behind `debug_assert!`
    // (or a future `#[cfg(not(debug_assertions))]` branch — see the release defmt
    // transport pattern in task_orchestration.rs) is only type-checked here.
    // bsp/target-tests carry no such code, so re-linting them would just repeat
    // the first pass.
    if has_bootloader() {
        cmd!(sh, "cargo clippy -p app -p boot --release -- -D warnings").run()?;
    } else {
        cmd!(sh, "cargo clippy -p app --release -- -D warnings").run()?;
    }
    Ok(())
}

fn flash_app(sh: &xshell::Shell, profile: &str) -> Result<(), anyhow::Error> {
    flash(sh, "app", profile)
}

fn flash_boot(sh: &xshell::Shell, profile: &str) -> Result<(), anyhow::Error> {
    if !has_bootloader() {
        return Ok(());
    }
    flash(sh, "boot", profile)
}

fn flash(sh: &xshell::Shell, package: &str, profile: &str) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir().join("cross"));
    match profile {
        "release" => cmd!(sh, "cargo flash -p {package} --release --chip {CHIP}").run()?,
        "debug" => cmd!(sh, "cargo flash -p {package} --chip {CHIP}").run()?,
        other => anyhow::bail!("unknown profile: {other}"),
    }
    Ok(())
}

/// Регион из `MEMORY {}`. Читается ради одного: `test host-target` нужен
/// адрес `PERSIST`, чтобы хост знал, откуда считывать счётчик запусков.
struct Region {
    origin: u64,
    // Длина в разборе есть, но никем пока не спрашивается: держать её здесь
    // дешевле, чем городить отдельный тип, когда она понадобится.
    #[allow(dead_code)]
    length: u64,
}

/// `MEMORY { NAME (attrs) : ORIGIN = 0x..., LENGTH = 128K }` из `memory.x`.
/// Свой разбор, а не крейт: формат пишем мы сами (`chip-select.rhai`), а
/// нужны из него только адрес и размер.
fn parse_memory_regions(memory_x: &Path) -> Result<Vec<(String, Region)>, anyhow::Error> {
    let text = fs::read_to_string(memory_x)?;
    let mut regions = Vec::new();
    for line in text.lines() {
        let line = line.trim();
        if line.starts_with("/*") || !line.contains("ORIGIN") || !line.contains("LENGTH") {
            continue;
        }
        let Some((name, rest)) = line.split_once('(') else {
            continue;
        };
        let Some((origin, length)) = rest.split_once("LENGTH") else {
            continue;
        };
        let Some((_, origin)) = origin.split_once("ORIGIN") else {
            continue;
        };
        let origin = origin.trim_start_matches([' ', '=']).trim();
        let length = length.trim_start_matches([' ', '=']).trim();
        if let (Some(origin), Some(length)) = (parse_size(origin), parse_size(length)) {
            regions.push((name.trim().to_owned(), Region { origin, length }));
        }
    }
    Ok(regions)
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
    digits.trim().parse::<u64>().ok().map(|n| n * multiplier)
}

fn region<'a>(regions: &'a [(String, Region)], name: &str) -> Option<&'a Region> {
    regions
        .iter()
        .find(|(region_name, _)| region_name == name)
        .map(|(_, region)| region)
}
fn root_dir() -> PathBuf {
    let mut xtask_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    xtask_dir.pop();
    xtask_dir
}
