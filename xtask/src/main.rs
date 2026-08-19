use std::{env, fs, path::Path, path::PathBuf};

use anyhow::Context;
use object::{Object, ObjectSection};
use xshell::cmd;

const CHIP: &str = "{{chip}}";
/// Целевой triple прошивки — тот же, что в `cross/.cargo/config.toml`. Нужен,
/// чтобы найти собранный ELF (`cross/target/<TARGET>/<profile>/app`) и чтобы
/// `cargo xtask setup` поставил ровно тот target, под который собирается
/// проект.
const TARGET: &str = "{{target}}";

/// Профиль по умолчанию для команд, где он необязателен. `debug` — потому что
/// команды с профилем (`run`/`flash`/`attach`/`size`) чаще всего зовут во
/// время разработки, а не при выпуске.
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
        ["run"] => run(&sh, DEFAULT_PROFILE),
        ["run", profile] => run(&sh, profile),
        ["flash"] => flash_all(&sh, DEFAULT_PROFILE),
        ["flash", profile] => flash_all(&sh, profile),
        ["attach"] => attach(&sh, DEFAULT_PROFILE),
        ["attach", profile] => attach(&sh, profile),
        ["size"] => size(&sh, DEFAULT_PROFILE),
        ["size", profile] => size(&sh, profile),
        ["reset"] => probe_rs(&sh, "reset"),
        ["erase"] => erase(&sh),
        ["lint"] => lint_host(&sh),
        ["lint", "cross"] => lint_cross(&sh),
        _ => {
            usage();
            Ok(())
        }
    }
}

fn usage() {
    println!("USAGE cargo xtask setup                      # target, probe-rs, flip-link, nextest");
    println!("      cargo xtask build                      # cross: debug + release");
    println!("      cargo xtask run [debug|release]        # прошить и смотреть defmt-лог");
    println!("      cargo xtask flash [debug|release]      # прошить без дебаггера");
    println!("      cargo xtask attach [debug|release]     # лог уже прошитой платы");
    println!("      cargo xtask size [debug|release]       # занятость FLASH/RAM");
    println!("      cargo xtask reset                      # сбросить плату");
    println!(
        "      cargo xtask erase                      # стереть флеш (спасает от цикла паник)"
    );
    println!("      cargo xtask lint [cross]");
    println!("      cargo xtask test [host|target|host-target|all]");
}

/// Ставит всё, без чего `build`/`run` падают с невнятной ошибкой линкера или
/// `no such command`. Уже установленное `cargo install` пропускает сам.
fn setup(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    // Компоненты — не «на всякий случай»: профиль rustup бывает minimal (так
    // устроены и оба CI-образа), и тогда `cargo xtask lint` сразу после setup
    // падал бы на `cargo fmt`/`cargo clippy` с "no such command".
    cmd!(sh, "rustup component add rustfmt clippy").run()?;
    cmd!(sh, "rustup target add {TARGET}").run()?;
    cmd!(sh, "cargo install probe-rs-tools --locked").run()?;
    cmd!(sh, "cargo install flip-link --locked").run()?;
    cmd!(sh, "cargo install cargo-nextest --locked").run()?;
    Ok(())
}

fn run(sh: &xshell::Shell, profile: &str) -> Result<(), anyhow::Error> {
    flash_boot(sh, profile)?;
    let _p = sh.push_dir(root_dir().join("cross/app"));
    match profile {
        "release" => cmd!(sh, "cargo run --release").run()?,
        "debug" => cmd!(sh, "cargo run").run()?,
        other => anyhow::bail!("unknown profile: {other}"),
    }
    Ok(())
}

fn flash_all(sh: &xshell::Shell, profile: &str) -> Result<(), anyhow::Error> {
    flash_boot(sh, profile)?;
    flash_app(sh, profile)?;
    Ok(())
}

/// Подключается к уже прошитой плате и печатает её defmt-лог, ничего не
/// перепрошивая: `run` для этого пришлось бы стирать и заливать образ заново,
/// теряя состояние, за которым как раз и наблюдают.
fn attach(sh: &xshell::Shell, profile: &str) -> Result<(), anyhow::Error> {
    let elf = artifact_path("app", profile)?;
    let elf = elf.display().to_string();
    let _p = sh.push_dir(root_dir().join("cross"));
    cmd!(sh, "probe-rs attach --chip {CHIP} {elf}").run()?;
    Ok(())
}

fn probe_rs(sh: &xshell::Shell, subcommand: &str) -> Result<(), anyhow::Error> {
    cmd!(sh, "probe-rs {subcommand} --chip {CHIP}").run()?;
    Ok(())
}

/// Стереть флеш целиком. Единственная команда здесь с
/// `--connect-under-reset`, и не для порядка: `erase` зовут не от хорошей
/// жизни. Типичный повод — прошивка, которая паникует на старте: в release
/// паникёр после сохранения причины делает `sys_reset`, и плата уходит в цикл
/// «старт → паника → сброс». К такой плате обычное подключение не успевает
/// (`An ARM specific error occurred` / таймаут), и залить исправленный образ
/// нельзя — сначала надо её стереть. С удержанным reset'ом probe-rs
/// захватывает ядро до того, как оно снова дойдёт до паники. Проверено на
/// STM32F3Discovery: без флага стереть зациклившуюся плату не удавалось.
fn erase(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    cmd!(sh, "probe-rs erase --chip {CHIP} --connect-under-reset").run()?;
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
/// bootloader'а нет — свежей или после `cargo xtask erase`, — без этой строки
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

/// Сколько флеша и RAM занимает собранный образ и сколько под него отведено в
/// `memory.x`. Линкер ловит только переполнение — а знать запас полезно
/// заранее, тем более что размер `ACTIVE` под OTA-схемой заметно меньше всего
/// флеша чипа.
fn size(sh: &xshell::Shell, profile: &str) -> Result<(), anyhow::Error> {
    {
        let _p = sh.push_dir(root_dir().join("cross"));
        match profile {
            "release" => cmd!(sh, "cargo build --release").run()?,
            "debug" => cmd!(sh, "cargo build").run()?,
            other => anyhow::bail!("unknown profile: {other}"),
        }
    }

    let mut packages = vec!["app"];
    if has_bootloader() {
        packages.push("boot");
    }
    for package in packages {
        let elf = artifact_path(package, profile)?;
        let memory_x = root_dir().join("cross").join(package).join("memory.x");
        let regions = parse_memory_regions(&memory_x)
            .with_context(|| format!("разобрать {}", memory_x.display()))?;
        let ram = region(&regions, "RAM");
        let usage =
            section_usage(&elf, ram).with_context(|| format!("разобрать ELF {}", elf.display()))?;

        println!(
            "{package} ({profile}): FLASH {}   RAM {}",
            report(usage.flash, region(&regions, "FLASH").map(|r| r.length)),
            report(usage.ram, ram.map(|r| r.length)),
        );
    }
    Ok(())
}

struct SectionUsage {
    flash: u64,
    ram: u64,
}

struct Region {
    origin: u64,
    length: u64,
}

impl Region {
    fn contains(&self, address: u64) -> bool {
        address >= self.origin && address < self.origin + self.length
    }
}

/// Суммирует ALLOC-секции ELF: во флеш попадает всё, что там физически лежит
/// (`.vector_table`/`.text`/`.rodata` плюс образ инициализированных данных),
/// в RAM — всё записываемое (`.data` + `.bss` + `.uninit`). `.data` считается
/// в обе стороны, и это не ошибка: её образ занимает флеш, а копия — RAM.
///
/// Записываемая секция засчитывается в RAM только если её адрес попал в
/// границы одноимённого региона: `.persist` живёт в PERSIST, а данные,
/// разложенные по CCMRAM/AXISRAM, — в своих регионах, и сложить их вместе
/// значило бы показать занятость RAM больше настоящей.
fn section_usage(elf: &Path, ram: Option<&Region>) -> Result<SectionUsage, anyhow::Error> {
    let data = fs::read(elf).with_context(|| format!("прочитать {}", elf.display()))?;
    let file = object::File::parse(&*data)?;

    let mut usage = SectionUsage { flash: 0, ram: 0 };
    for section in file.sections() {
        let object::SectionFlags::Elf { sh_flags, sh_type } = section.flags() else {
            continue;
        };
        // Не ALLOC — отладочная информация и таблицы символов: в устройство
        // они не попадают вовсе.
        if sh_flags.0 & object::elf::SHF_ALLOC.0 == 0 {
            continue;
        }
        let size = section.size();
        if sh_flags.0 & object::elf::SHF_WRITE.0 != 0
            && ram.is_none_or(|ram| ram.contains(section.address()))
        {
            usage.ram += size;
        }
        // `.bss`/`.uninit` места в файле не занимают (SHT_NOBITS) — во флеш
        // идёт только то, у чего есть содержимое.
        if sh_type != object::elf::SHT_NOBITS {
            usage.flash += size;
        }
    }
    Ok(usage)
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

fn report(used: u64, available: Option<u64>) -> String {
    match available {
        Some(available) if available > 0 => format!(
            "{} / {} ({}%)",
            human(used),
            human(available),
            used * 100 / available,
        ),
        _ => human(used),
    }
}

fn human(bytes: u64) -> String {
    if bytes >= 1024 {
        format!("{}.{} KiB", bytes / 1024, (bytes % 1024) * 10 / 1024)
    } else {
        format!("{bytes} B")
    }
}

fn artifact_path(package: &str, profile: &str) -> Result<PathBuf, anyhow::Error> {
    let profile_dir = match profile {
        "release" => "release",
        "debug" => "debug",
        other => anyhow::bail!("unknown profile: {other}"),
    };
    Ok(cross_target_dir()
        .join(TARGET)
        .join(profile_dir)
        .join(package))
}

/// Куда cargo кладёт артефакты `cross`. Обычно это `cross/target`, но
/// `CARGO_TARGET_DIR` перекрывает его глобально (так делают CI и инструменты,
/// которым нужен общий кеш) — а команды, ищущие собранный ELF (`size`,
/// `attach`, `test host-target`), должны находить его там же, где cargo его
/// оставил, а не там, где он лежит по умолчанию.
fn cross_target_dir() -> PathBuf {
    match env::var_os("CARGO_TARGET_DIR") {
        Some(dir) => PathBuf::from(dir),
        None => root_dir().join("cross").join("target"),
    }
}

fn root_dir() -> PathBuf {
    let mut xtask_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    xtask_dir.pop();
    xtask_dir
}
