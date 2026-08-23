use std::{env, fs, path::Path, path::PathBuf};

use anyhow::Context;
use xshell::cmd;

const CHIP: &str = "{{chip}}";
/// Целевой triple прошивки — тот же, что в `crates-cross/.cargo/config.toml`. Нужен,
/// чтобы `cargo xtask setup` поставил ровно тот target, под который собирается
/// проект.
const TARGET: &str = "{{target}}";

/// Профиль по умолчанию для `flash`: во время разработки прошивают чаще, чем
/// при выпуске.
const DEFAULT_PROFILE: &str = "debug";

/// Минимальная единица записи во flash этого чипа. Подставляется при
/// генерации, как и всё остальное про раскладку. Нужна host-target тесту:
/// состояние bootloader'а — это `WRITE_SIZE` байт одинаковой магии, короче
/// записать нельзя.
const WRITE_SIZE: &str = "{{write_size}}";

/// Размер страницы стирания в терминах `embassy-boot` (максимальный сектор
/// чипа). Ею bootloader переносит разделы, и на неё же смещается прежний
/// образ при обмене — host-target тесту нужно знать, где его искать.
const PAGE_SIZE: &str = "{{page_size}}";

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
        // Аргументы уходят в chip-info как есть (блок, вывод, `--snippet`,
        // `--check`) — разбирает их он, здесь незачем знать его ключи.
        ["pins", args @ ..] => pins(&sh, args),
        ["panic"] => panic_dump(&sh),
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
    println!(
        "      cargo xtask build                      # debug + release, app.bin, размер, подпись"
    );
    println!("      cargo xtask flash [debug|release]      # прошить bootloader + приложение");
    println!("      cargo xtask lint [cross]");
    println!("      cargo xtask test [host|target|host-target|all]");
    println!("      cargo xtask pins [БЛОК|ПИН]            # справочник по чипу: SPI1, PA9");
    println!(
        "      cargo xtask pins БЛОК --snippet        # заготовка bind_interrupts!/assign_resources!"
    );
    println!(
        "      cargo xtask pins --check               # не занят ли отладочный порт в resources.rs"
    );
    println!("      cargo xtask panic                      # причина последней паники с платы");
    if signing_enabled() {
        println!();
        println!("Отдельных команд для подписи нет: `build` сам создаёт ключевую пару при первой");
        println!("сборке, кладёт открытый ключ в прошивку и подписывает release-образ.");
    }
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
    // Ключ приводится в порядок ДО сборки, а не после: открытый попадает в
    // прошивку через build.rs крейта bsp, то есть должен существовать к моменту
    // компиляции.
    //
    // И только там, где есть что подписывать: без bootloader'а раздела `DFU` не
    // существует, доставлять образ некуда, а пара ключей в корне проекта была
    // бы вредна — открытый просился бы в коммит, закрытый охранял бы пустоту.
    let seed = if signing_enabled() && has_bootloader() {
        ensure_signing_key(&root_dir())?
    } else {
        None
    };

    {
        let _p = sh.push_dir(root_dir().join("crates-cross"));
        cmd!(sh, "cargo build").run()?;
        cmd!(sh, "cargo build --release").run()?;
    }

    let elf = cross_target_dir().join(TARGET).join("release").join("app");
    anyhow::ensure!(
        elf.exists(),
        "сборка прошла, но ELF по пути {} не найден. Так бывает, если каталог сборки задан \
         не переменной CARGO_TARGET_DIR, а `[build] target-dir` в .cargo/config.toml — его \
         xtask не читает",
        elf.display(),
    );
    let (base, image) = raw_image(&elf)?;

    // Размер печатается всегда: он одинаково полезен и проекту без OTA, где
    // приложению достаётся весь флеш. Заодно сверяется адрес, с которого образ
    // начинается.
    report_image(base, image.len() as u64)?;

    // А файл нужен только там, где есть куда его доставлять: `.bin` — это
    // ровно то, что уезжает в раздел `DFU`.
    if has_bootloader() {
        let path = elf.with_extension("bin");
        fs::write(&path, &image).with_context(|| format!("записать {}", path.display()))?;
        println!("образ для OTA: {}", path.display());

        if let Some(seed) = seed {
            sign_image(&path, &image, &seed)?;
        }
    }
    Ok(())
}

/// Каталог сборки `cross`-воркспейса.
///
/// Не просто `crates-cross/target`: `CARGO_TARGET_DIR` уводит артефакты в
/// другое место, и это не экзотика — так собирает `template-check`, деля один
/// кеш между конфигурациями, и так устроены CI с общим кешем. Строя путь
/// жёстко, `build` не находил бы ELF ровно там, где сборка идёт чаще всего.
fn cross_target_dir() -> PathBuf {
    let cross = root_dir().join("crates-cross");
    match env::var_os("CARGO_TARGET_DIR") {
        // Относительный путь cargo считает от СВОЕГО рабочего каталога, а
        // `cargo build` выше запускается из `crates-cross`. Разрешая его от
        // корня, мы искали бы ELF на уровень выше, чем он лёг, — причём уже
        // после успешной компиляции, что выглядит особенно загадочно.
        Some(dir) if Path::new(&dir).is_relative() => cross.join(dir),
        Some(dir) => PathBuf::from(dir),
        None => cross.join("target"),
    }
}

/// Регионы из `memory.x` приложения.
///
/// Один разбор на три места (`build`, `test host-target`, `panic`): путь и
/// текст ошибки уже были скопированы, и следующая правка раскладки нашлась бы
/// не во всех копиях.
fn app_memory_regions() -> Result<Vec<(String, Region)>, anyhow::Error> {
    let memory_x = root_dir().join("crates-cross").join("app").join("memory.x");
    parse_memory_regions(&memory_x).with_context(|| format!("разобрать {}", memory_x.display()))
}

/// Раздел, в который линкуется приложение, и как его называть в выводе.
///
/// Приложение линкуется в `ACTIVE`, а если такого региона нет — в `FLASH`.
/// Причём в посчитанном при генерации файле `FLASH` приложения это и ЕСТЬ
/// раздел `ACTIVE`: линкеру он отдан под этим именем. Отсюда и второе имя в
/// паре — по наличию `DFU` видно, раздел перед нами или весь флеш, а сказать
/// «из 128 KiB FLASH» про чип с 512 KiB значило бы соврать.
fn app_region(regions: &[(String, Region)]) -> Option<(&'static str, &Region)> {
    let partitioned = region(regions, "DFU").is_some();
    region(regions, "ACTIVE")
        .map(|active| ("ACTIVE", active))
        .or_else(|| {
            region(regions, "FLASH")
                .map(|flash| (if partitioned { "ACTIVE" } else { "FLASH" }, flash))
        })
}

/// Печатает, сколько образ занимает от раздела, и проверяет, что он вообще
/// лёг туда, куда должен.
///
/// Проверок две, и обе про то, чего не поймает никто другой.
///
/// Адрес: `base` — минимальный физический адрес сегмента ELF, и он обязан
/// совпасть с началом раздела. Разойтись они могут тише, чем кажется:
/// `-C link-arg=--nmagic` в `crates-cross/.cargo/config.toml` держит заголовки
/// ELF вне первого загружаемого сегмента, а без него линкер вкладывает их
/// внутрь, и `p_paddr` уезжает ниже `ORIGIN`. Флаг лежит в списке, который с
/// OTA никто не связывает, — убери его, и `.bin` получит лишний префикс,
/// каждый байт сместится, а подпись честно подтвердит испорченный образ.
///
/// Размер: образ, не помещающийся в раздел, — это ошибка, а не повод для
/// предупреждения. Линкер её поймает не всегда: раскладку `memory.x` могли
/// поправить руками после сборки.
fn report_image(base: u64, size: u64) -> Result<(), anyhow::Error> {
    let regions = app_memory_regions()?;
    let Some((name, partition)) = app_region(&regions) else {
        // Раскладку заполняли руками и назвали регионы иначе — сравнивать не с
        // чем, но сам размер всё равно скажем.
        println!("app: {:.1} KiB", size as f64 / 1024.0);
        return Ok(());
    };

    anyhow::ensure!(
        base == partition.origin,
        "образ начинается с {base:#x}, а раздел {name} — с {:#x}. Раскладка и линковка разошлись: \
         подписывать и заливать такой образ нельзя, каждый его байт сместится относительно того, \
         что ждёт устройство. Проверьте memory.x и rustflags в crates-cross/.cargo/config.toml \
         (в частности `--nmagic`)",
        partition.origin,
    );

    let percent = size * 100 / partition.length.max(1);
    println!(
        "app: {:.1} KiB из {:.1} KiB {name} ({percent}%)",
        size as f64 / 1024.0,
        partition.length as f64 / 1024.0,
    );
    anyhow::ensure!(
        size <= partition.length,
        "образ не помещается в раздел {name}: {size} байт против {}",
        partition.length,
    );
    if percent >= 90 {
        println!("ВНИМАНИЕ: до границы раздела осталось меньше десятой части");
    }
    Ok(())
}

/// Подписывает образ: SHA-512 от его байтов, подпись самого хеша — так
/// проверяет `verify_and_mark_updated` в embassy-boot, так же считает и
/// устройство.
fn sign_image(image_path: &Path, image: &[u8], seed: &[u8; 32]) -> Result<(), anyhow::Error> {
    let digest = salty::Sha512::new().updated(image).finalize();
    let signature = salty::Keypair::from(seed).sign(&digest).to_bytes();

    let path = image_path.with_extension("bin.sig");
    fs::write(&path, signature).with_context(|| format!("записать {}", path.display()))?;

    println!("подпись: {} (64 байта)", path.display());
    println!(
        "устройству нужны обе величины: подпись и длина образа ({} байт) — их принимает \
         Ota::verify_and_mark_updated",
        image.len(),
    );
    Ok(())
}

/// Whether this project has a bootloader at all — substituted at generation
/// time. `"false"` when the OTA layout does not fit the chip's flash: then
/// `crates-cross/boot` is not part of the generated project (see `chip-select.rhai`),
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

    let regions = app_memory_regions()?;
    let persist = region(&regions, "PERSIST").context(
        "в crates-cross/app/memory.x нет региона PERSIST — на этом чипе host-target тесту \
         не за что зацепиться",
    )?;

    // Разделы OTA — ради теста полного цикла обновления: он пишет образ в
    // `DFU`, просит смену разделов через `BOOTLOADER_STATE` и смотрит, что
    // оказалось в `ACTIVE`. Раздел приложения ищется тем же `app_region`, что и
    // в `build`: разъедься эти два места, `build` мерил бы один регион, а тест
    // проверял другой.
    let active = app_region(&regions).map(|(_, partition)| partition);
    let dfu = region(&regions, "DFU");
    let state = region(&regions, "BOOTLOADER_STATE");
    let ram = region(&regions, "RAM");

    {
        let _env_chip = sh.push_env("HOST_TARGET_CHIP", CHIP);
        let _env_persist =
            sh.push_env("HOST_TARGET_PERSIST_ADDR", format!("{:#x}", persist.origin));
        let _env_ota = ota_env(sh, active, dfu, state, ram);
        let _p = sh.push_dir(root_dir().join("crates-host").join("host-target-tests"));
        // В один поток: пробник у платы один, а nextest по умолчанию гоняет
        // тесты параллельно — два `probe-rs` на одной цели дерутся за неё и
        // падают с невнятной ошибкой захвата.
        cmd!(sh, "cargo nextest run --test-threads 1").run()?;
    }
    Ok(())
}

/// Адреса разделов OTA в окружение теста — если они вообще есть.
///
/// Возвращает guard'ы `xshell`: переменные живут, пока жив результат. Пустой
/// вектор (нет bootloader'а или региона) означает, что тест полного цикла
/// обновления сам себя пропустит — проверять там нечего.
fn ota_env<'a>(
    sh: &'a xshell::Shell,
    active: Option<&Region>,
    dfu: Option<&Region>,
    state: Option<&Region>,
    ram: Option<&Region>,
) -> Vec<xshell::PushEnv<'a>> {
    let (Some(active), Some(dfu), Some(state), Some(ram)) = (active, dfu, state, ram) else {
        return Vec::new();
    };
    // Пустой `page_size` означает, что раскладку считал не шаблон, а человек
    // (см. chip-select.rhai): размер страницы стирания тогда неизвестен, а
    // без него тест обмена искал бы прежний образ не по тому адресу — он
    // уезжает ровно на страницу. Лучше пропустить проверку, чем получить
    // падение, обвиняющее bootloader.
    if PAGE_SIZE.is_empty() {
        return Vec::new();
    }
    vec![
        sh.push_env("HOST_TARGET_ACTIVE_ADDR", format!("{:#x}", active.origin)),
        sh.push_env("HOST_TARGET_DFU_ADDR", format!("{:#x}", dfu.origin)),
        sh.push_env("HOST_TARGET_STATE_ADDR", format!("{:#x}", state.origin)),
        // Длина нужна не для красоты: `mark_updated()` стирает раздел
        // состояния ЦЕЛИКОМ, и тест обязан делать то же самое. Раздел
        // многосекторный на чипах с мелкими страницами (журнал прогресса —
        // слово на страницу ACTIVE в каждом из четырёх проходов), и стирание
        // одного первого сектора оставило бы там прошлый журнал.
        sh.push_env("HOST_TARGET_STATE_LEN", state.length.to_string()),
        // Вершина RAM — начальное значение указателя стека для образа,
        // который тест зальёт в DFU. Сам он стеком не пользуется, но
        // Cortex-M читает это слово при старте раньше первой инструкции.
        sh.push_env(
            "HOST_TARGET_RAM_END",
            format!("{:#x}", ram.origin + ram.length),
        ),
        sh.push_env("HOST_TARGET_WRITE_SIZE", WRITE_SIZE),
        sh.push_env("HOST_TARGET_PAGE_SIZE", PAGE_SIZE),
    ]
}

/// Тесты внутри МК. Bootloader заливается первым по той же причине, что и в
/// `test_host_target`: тестовый образ линкуется в `ACTIVE` (его `memory.x` —
/// копия app'ового), а `probe-rs run` после заливки сбрасывает чип, и
/// управление получает не тест, а то, что лежит с базы flash. На плате, где
/// bootloader'а нет — свежей или после `probe-rs erase`, — без этой строки
/// не выполнился бы ни один тест.
fn test_target(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    flash_boot(sh, "debug")?;
    let _p = sh.push_dir(root_dir().join("crates-cross/target-tests"));
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
    let _p = sh.push_dir(root_dir().join("crates-cross"));
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

/// Чип-фича `embassy-stm32`/`stm32-metapac`, выбранная при генерации. Нужна
/// не для запуска, а чтобы отличить сгенерированный проект от самого
/// репозитория шаблона — см. [`pins`].
const CHIP_FEATURE: &str = "{{chip_feature}}";

/// Справочник по чипу: `cargo xtask pins SPI1`, `cargo xtask pins PA9`, без
/// аргумента — список блоков с выводами.
///
/// Обёртка здесь оправдана (в отличие от убранных обёрток над `probe-rs`):
/// путь к манифесту `chip-info` относительный, и прямой вызов `cargo run
/// --manifest-path crates-host/chip-info/Cargo.toml` работал бы только из корня проекта.
///
/// Отдельный крейт, а не подкоманда прямо здесь, — вынужденно: `stm32-metapac`
/// выбирает чип Cargo-фичей, то есть в его манифесте живёт Liquid, а манифест
/// с Liquid не резолвится. Будь он членом корневого workspace, в репозитории
/// шаблона перестали бы работать `cargo xtask lint` и `test host`. Подробнее —
/// в doc-комментарии `crates-host/chip-info/src/main.rs`.
fn pins(sh: &xshell::Shell, args: &[&str]) -> Result<(), anyhow::Error> {
    // В самом репозитории шаблона плейсхолдер не подставлен — там chip-info не
    // собирается вовсе, и невнятная ошибка Liquid-парсинга манифеста лучше
    // объясняется здесь.
    if CHIP_FEATURE.starts_with('{') {
        anyhow::bail!(
            "`cargo xtask pins` работает только в сгенерированном проекте: в репозитории \
             шаблона в crates-host/chip-info/Cargo.toml вместо чип-фичи стоит Liquid-плейсхолдер. \
             Проверять эту команду — через `cargo run --manifest-path chip-data-gen/Cargo.toml \
             --bin template-check`"
        );
    }
    let _p = sh.push_dir(root_dir());
    // Без `--quiet`: первый запуск собирает stm32-metapac (~15 секунд), и
    // молчащий терминал выглядел бы как зависание.
    cmd!(
        sh,
        "cargo run --manifest-path crates-host/chip-info/Cargo.toml -- {args...}"
    )
    .run()?;
    Ok(())
}

/// Подписываются ли OTA-образы — ответ, данный при генерации.
const SIGNED: &str = "{{signed}}";

/// Файл с закрытым ключом подписи. Лежит в корне проекта и внесён в
/// `.gitignore`: закоммитить его — то же самое, что не подписывать образы
/// вовсе.
const SIGNING_KEY_FILE: &str = "ota-signing-key.bin";

/// Открытый ключ, которым устройство проверяет подпись. В отличие от закрытого
/// — коммитится: именно он попадает в прошивку (его читает
/// `crates-cross/bsp/build.rs`), и без него собранный образ не примет ни одно
/// устройство.
const PUBLIC_KEY_FILE: &str = "ota-public-key.bin";

/// Приводит пару ключей в согласованное состояние и возвращает закрытый.
///
/// Четыре случая, и только один из них — отказ:
///
/// * нет обоих файлов — создаётся пара (первая сборка проекта с подписью);
/// * есть закрытый, нет открытого — открытый выводится из закрытого. Корень
///   доверия при этом не меняется, поэтому делается тихо;
/// * есть оба — сверяется, что открытый выведен из этого закрытого;
/// * есть открытый, нет закрытого — `Ok(None)`: собрать можно, подписать нечем.
///
/// Последний случай — это каждый CI-прогон и каждый свежий клон: закрытый ключ
/// лежит в `.gitignore`, открытый коммитится. Выпускать там новый ключ нельзя
/// (прошивка перестала бы приниматься уже прошитыми устройствами), но и падать
/// не за что: для КОМПИЛЯЦИИ закрытый ключ не нужен вовсе — в прошивку уезжает
/// открытый. Поэтому сборка идёт дальше, а подпись пропускается с
/// предупреждением.
fn ensure_signing_key(root: &Path) -> Result<Option<[u8; 32]>, anyhow::Error> {
    let private_path = root.join(SIGNING_KEY_FILE);
    let public_path = root.join(PUBLIC_KEY_FILE);

    if !private_path.exists() {
        if public_path.exists() {
            println!(
                "{PUBLIC_KEY_FILE} есть, а {SIGNING_KEY_FILE} нет — образ собран, но НЕ подписан."
            );
            println!(
                "Так выглядит свежий клон и любой CI: закрытый ключ в .gitignore и остаётся у \
                 того, кто выпускает обновления."
            );
            println!(
                "Новый ключ здесь не создаётся намеренно: устройства, прошитые прежним, \
                 обновлений с ним не примут. Нужен именно новый корень доверия — удалите \
                 {PUBLIC_KEY_FILE}."
            );
            return Ok(None);
        }

        let mut seed = [0u8; 32];
        getrandom::fill(&mut seed).context("не удалось получить случайные байты от ОС")?;
        write_private(&private_path, &seed)
            .with_context(|| format!("записать {}", private_path.display()))?;
        write_public(&public_path, &seed)?;

        println!("создана ключевая пара для подписи образов:");
        println!(
            "  {} — закрытый, в .gitignore. Потеряете его — выпускать",
            private_path.display()
        );
        println!("  обновления для уже прошитых устройств станет нечем.");
        println!(
            "  {} — открытый, попадает в прошивку. Закоммитьте его.",
            public_path.display()
        );
        return Ok(Some(seed));
    }

    let seed = read_seed(&private_path)?;

    if public_path.exists() {
        let stored = fs::read(&public_path)
            .with_context(|| format!("прочитать {}", public_path.display()))?;
        anyhow::ensure!(
            stored == salty::Keypair::from(&seed).public.to_bytes(),
            "{PUBLIC_KEY_FILE} не соответствует {SIGNING_KEY_FILE}: открытый ключ выведен из \
             другого закрытого. Устройство, прошитое этим открытым ключом, не примет образ, \
             подписанный имеющимся закрытым",
        );
    } else {
        // Открытый восстанавливаем из закрытого, и это не смена корня доверия
        // — ключ тот же, просто файл не дошёл (свежий клон, чистка каталога).
        write_public(&public_path, &seed)?;
        println!(
            "{} восстановлен из закрытого ключа — закоммитьте его",
            public_path.display()
        );
    }

    Ok(Some(seed))
}

/// Байты, которыми заполняются пропуски между сегментами: стёртое состояние
/// NOR-флеша. Значение существенно — содержимое пропусков уезжает в раздел
/// `DFU` вместе с остальным образом, и разойдись оно со стёртым флешем, хеш
/// раздела на устройстве не совпал бы с посчитанным на хосте.
const ERASED_FLASH: u8 = 0xFF;

/// Укладывает сегменты в непрерывный образ, начиная с самого младшего адреса.
///
/// Отдельная функция от чтения ELF, потому что здесь единственное, чему есть
/// чем сломаться: смещения, дыры, наложения. Читать ELF умеет `object`, а вот
/// проверить укладку без такой функции было бы нечем — для этого понадобился
/// бы настоящий ELF-файл в тестах.
fn image_from_segments(segments: &[(u64, Vec<u8>)]) -> Result<(u64, Vec<u8>), anyhow::Error> {
    let Some(base) = segments.iter().map(|(address, _)| *address).min() else {
        anyhow::bail!("в ELF нет ни одного загружаемого сегмента");
    };
    let end = segments
        .iter()
        .map(|(address, bytes)| address + bytes.len() as u64)
        .max()
        .unwrap_or(base);

    let mut image = vec![ERASED_FLASH; (end - base) as usize];
    let mut filled = vec![false; image.len()];
    for (address, bytes) in segments {
        let offset = (address - base) as usize;
        for (index, byte) in bytes.iter().enumerate() {
            anyhow::ensure!(
                !filled[offset + index],
                "сегменты ELF накладываются по адресу {:#x}",
                base + (offset + index) as u64,
            );
            image[offset + index] = *byte;
            filled[offset + index] = true;
        }
    }
    Ok((base, image))
}

/// Сырой образ: то, что уезжает во флеш, без секций и символов ELF.
///
/// Разбор идёт через `ElfFile32` и заголовки программы, а не через общий
/// `object::File`: тому доступен только виртуальный адрес сегмента
/// (`ObjectSegment::address`), а нужен физический. Для `.data` они разные —
/// секция исполняется из RAM, но лежит во флеше, откуда её копирует
/// `cortex-m-rt` при старте. Возьми мы виртуальный, в образе появилась бы
/// дыра в сотни мегабайт между флешем и RAM.
fn raw_image(elf: &Path) -> Result<(u64, Vec<u8>), anyhow::Error> {
    use object::elf::PT_LOAD;
    use object::read::elf::{ElfFile32, FileHeader, ProgramHeader};

    let bytes = fs::read(elf).with_context(|| format!("прочитать {}", elf.display()))?;
    let file = ElfFile32::<object::Endianness>::parse(&*bytes)
        .with_context(|| format!("разобрать {}", elf.display()))?;
    let endian = file.endian();

    let mut segments = Vec::new();
    for header in file.elf_header().program_headers(endian, &*bytes)? {
        if header.p_type(endian) != PT_LOAD {
            continue;
        }
        // `p_filesz`, а не `p_memsz`: разница между ними — `.bss`, которого в
        // файле нет и во флеш писать нечего (его обнуляет cortex-m-rt).
        let data = header
            .data(endian, &*bytes)
            .map_err(|()| anyhow::anyhow!("сегмент ELF выходит за границы файла"))?;
        if data.is_empty() {
            continue;
        }
        segments.push((u64::from(header.p_paddr(endian)), data.to_vec()));
    }
    image_from_segments(&segments)
}

/// Закрытый ключ — 32 байта seed, из которых `salty` разворачивает пару.
fn read_seed(path: &Path) -> Result<[u8; 32], anyhow::Error> {
    let bytes = fs::read(path).with_context(|| format!("прочитать {}", path.display()))?;
    bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("{} должен быть ровно 32 байта", path.display()))
}

fn write_public(path: &Path, seed: &[u8; 32]) -> Result<(), anyhow::Error> {
    let public = salty::Keypair::from(seed).public.to_bytes();
    fs::write(path, public).with_context(|| format!("записать {}", path.display()))
}

/// Сравнение с `"true"`, а не с `"false"`: в неподставленном шаблоне (где
/// константа — сам плейсхолдер) команды должны отказывать, а не делать вид,
/// что подпись включена.
fn signing_enabled() -> bool {
    SIGNED == "true"
}

/// Пишет файл, закрытый для всех, кроме владельца.
///
/// Права выставляются при создании, а не после: между `write` и `chmod` файл
/// с приватным ключом успел бы полежать читаемым для всех. На Windows прав
/// POSIX нет — там файл создаётся обычным способом, и защита сводится к тому,
/// что он в `.gitignore` и лежит только у вас.
fn write_private(path: &Path, bytes: &[u8]) -> Result<(), std::io::Error> {
    use std::io::Write;

    let mut options = fs::OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)
}

/// Магия, которой `panic-persist` помечает сохранённый дамп (первое слово
/// региона `PANIC`, дальше длина сообщения и его байты). Значение — из самого
/// крейта: считать формат приходится здесь, потому что читает его хост, а
/// прошивке в этот момент, как правило, не до того.
const PANIC_MAGIC: u32 = 0x0FAC_ADE0;

/// Причина последней паники, снятая с платы без перепрошивки и без
/// RTT-сессии.
///
/// Зачем отдельная команда, если `main` и так печатает дамп при старте: чтобы
/// увидеть эту печать, нужен пробник, подключённый **в момент** старта. Под
/// отладкой же плата после паники не стартует вовсе — хендлер заканчивается
/// `udf()`, ядро стоит в HardFault, — и дамп лежит в RAM, пока его никто не
/// прочитал. Этот случай команда и закрывает: пришли к зависшей плате,
/// спросили причину.
///
/// Ограничение прямое следствие того же: если приложение успело стартовать
/// (release-профиль, где хендлер делает `sys_reset`), оно дамп уже вычитало и
/// магию стёрло — `panic-persist` так устроен намеренно, чтобы одно падение
/// не показывалось вечно. Тогда причина есть только в defmt-логе того старта.
/// Нужно иначе — сохранять копию причины в `PERSIST` при старте (регион для
/// этого есть) и читать её отсюда; в шаблоне этого нет, потому что за него
/// платит каждый проект, а нужно оно не всем.
fn panic_dump(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let regions = app_memory_regions()?;
    let panic = region(&regions, "PANIC").context(
        "в crates-cross/app/memory.x нет региона PANIC — на этом чипе дамп паники негде хранить",
    )?;

    // Заголовок: магия и длина сообщения.
    let header = format!("{:#x}", panic.origin);
    let header = cmd!(sh, "probe-rs read --chip {CHIP} b32 {header} 2").read()?;
    let mut words = header.split_whitespace().map(parse_hex);
    let magic = words.next().unwrap_or(0);
    let length = words.next().unwrap_or(0);

    if magic != PANIC_MAGIC {
        println!("дампа нет: в начале PANIC не {PANIC_MAGIC:#010x}, а {magic:#010x}.");
        println!(
            "Либо плата не падала, либо приложение уже стартовало и вычитало дамп — тогда \
             причина ушла в defmt-лог того запуска."
        );
        return Ok(());
    }

    // Восемь байт заголовка не входят в сообщение. Длину всё равно
    // проверяем: в регионе могло оказаться что угодно, а `probe-rs read` с
    // мусорным числом слов ждал бы долго и молча.
    let available = panic.length.saturating_sub(8);
    if length == 0 || u64::from(length) > available {
        anyhow::bail!(
            "магия на месте, но длина сообщения ({length}) не помещается в PANIC ({available} \
             байт) — дамп повреждён"
        );
    }

    let start = format!("{:#x}", panic.origin + 8);
    let length = length.to_string();
    let body = cmd!(sh, "probe-rs read --chip {CHIP} b8 {start} {length}").read()?;
    let bytes = body
        .split_whitespace()
        .map(|byte| parse_hex(byte) as u8)
        .collect::<Vec<_>>();

    println!("причина последней паники ({} байт):", bytes.len());
    println!("{}", String::from_utf8_lossy(&bytes));
    Ok(())
}

/// Слово из вывода `probe-rs read`: он печатает hex без префикса.
fn parse_hex(word: &str) -> u32 {
    u32::from_str_radix(word, 16).unwrap_or(0)
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
    let _p = sh.push_dir(root_dir().join("crates-cross"));
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
    /// Нужна `panic`: по ней проверяется, что записанная в дампе длина
    /// сообщения вообще помещается в регион.
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
/// Корень проекта: два уровня вверх от манифеста, потому что сам `xtask`
/// лежит в `crates-host/`. Ошибиться тут легко и незаметно — все команды
/// строят пути от этого значения, и на уровень выше оно молча указывало бы в
/// `crates-host/`.
fn root_dir() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir.pop();
    dir
}

#[cfg(test)]
mod tests {
    use std::{fs, path::PathBuf};

    use super::{PUBLIC_KEY_FILE, SIGNING_KEY_FILE, ensure_signing_key};

    /// Свой каталог на каждый тест — и на каждый ПРОЦЕСС.
    ///
    /// Имя включает pid не для красоты: `std::env::temp_dir()` общий на машину,
    /// а тесты здесь пишут файлы ключей. Два прогона разом (свой `cargo xtask
    /// test host` рядом с `template-check`, который зовёт то же самое в каждом
    /// сгенерированном проекте, или просто два клона репозитория) чистили бы
    /// каталог друг у друга посреди работы. На общем CI-раннере каталог с
    /// фиксированным именем к тому же мог бы принадлежать другому пользователю.
    ///
    /// Возвращается страж: он удаляет каталог при выходе из теста, в том числе
    /// при панике. Иначе в `%TEMP%` оставался бы настоящий закрытый ключ
    /// ed25519 — пусть и от игрушечного проекта, привычка плохая.
    struct Scratch(PathBuf);

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
        fn path(&self) -> &std::path::Path {
            &self.0
        }
    }

    fn scratch(name: &str) -> Scratch {
        let dir = std::env::temp_dir().join(format!("xtask-key-{name}-{}", std::process::id()));
        if dir.exists() {
            fs::remove_dir_all(&dir).expect("очистить каталог прошлого прогона");
        }
        fs::create_dir_all(&dir).expect("создать временный каталог");
        Scratch(dir)
    }

    fn public_of(seed: &[u8; 32]) -> [u8; 32] {
        salty::Keypair::from(seed).public.to_bytes()
    }

    #[test]
    fn creates_a_pair_when_nothing_is_there() {
        let dir = scratch("empty");

        let seed = ensure_signing_key(dir.path())
            .expect("пара должна создаться")
            .expect("подписывать есть чем");

        let stored = fs::read(dir.path().join(SIGNING_KEY_FILE)).expect("закрытый ключ записан");
        let public = fs::read(dir.path().join(PUBLIC_KEY_FILE)).expect("открытый ключ записан");
        assert_eq!(stored, seed);
        assert_eq!(public, public_of(&seed));
    }

    #[test]
    fn restores_public_key_from_private() {
        let dir = scratch("no-public");
        let seed = [7u8; 32];
        fs::write(dir.path().join(SIGNING_KEY_FILE), seed).expect("положить закрытый ключ");

        let returned = ensure_signing_key(dir.path())
            .expect("открытый ключ должен восстановиться")
            .expect("подписывать есть чем");

        assert_eq!(returned, seed);
        let public = fs::read(dir.path().join(PUBLIC_KEY_FILE)).expect("открытый ключ записан");
        assert_eq!(public, public_of(&seed));
    }

    /// Ветка, по которой идёт каждая вторая и последующая сборка, — и до сих
    /// пор не проверенная ни одним тестом. Переверни сравнение ключей, и все
    /// остальные тесты остались бы зелёными, а любая нормальная сборка стала
    /// бы падать с «пара не соответствует».
    #[test]
    fn accepts_a_matching_pair_and_leaves_it_alone() {
        let dir = scratch("matching");
        let seed = [7u8; 32];
        fs::write(dir.path().join(SIGNING_KEY_FILE), seed).expect("положить закрытый ключ");
        fs::write(dir.path().join(PUBLIC_KEY_FILE), public_of(&seed))
            .expect("положить парный открытый");

        let returned = ensure_signing_key(dir.path())
            .expect("согласованная пара должна приниматься")
            .expect("подписывать есть чем");

        assert_eq!(returned, seed);
        let stored = fs::read(dir.path().join(SIGNING_KEY_FILE)).expect("закрытый ключ на месте");
        assert_eq!(stored, seed, "закрытый ключ трогать не надо");
    }

    /// Главный случай этого набора: так выглядит каждый CI-прогон и каждый
    /// свежий клон — открытый ключ в репозитории, закрытый в `.gitignore`.
    /// Собрать при этом можно (в прошивку уезжает открытый), а вот выпустить
    /// новый ключ нельзя: устройства, прошитые прежним, обновлений не примут.
    #[test]
    fn builds_without_signing_when_only_the_public_key_is_here() {
        let dir = scratch("no-private");
        fs::write(dir.path().join(PUBLIC_KEY_FILE), [1u8; 32]).expect("положить открытый ключ");

        let seed = ensure_signing_key(dir.path()).expect("сборке закрытый ключ не нужен");

        assert!(seed.is_none(), "подписывать здесь нечем");
        assert!(
            !dir.path().join(SIGNING_KEY_FILE).exists(),
            "закрытый ключ не должен появиться: это был бы новый корень доверия"
        );
    }

    #[test]
    fn packs_segments_and_fills_gaps_with_erased_flash() {
        // Два сегмента с дырой в четыре байта между ними.
        let segments = vec![(0x0800_0000, vec![1, 2]), (0x0800_0006, vec![3])];

        let (base, image) = super::image_from_segments(&segments).expect("сегменты укладываются");

        assert_eq!(base, 0x0800_0000);
        assert_eq!(image, vec![1, 2, 0xFF, 0xFF, 0xFF, 0xFF, 3]);
    }

    #[test]
    fn refuses_overlapping_segments() {
        // Наложение означает, что ELF описывает два разных содержимого для
        // одного адреса: молча выбрать одно из них нельзя.
        let segments = vec![(0x0800_0000, vec![1, 2, 3]), (0x0800_0002, vec![4])];

        super::image_from_segments(&segments).expect_err("наложение должно отвергаться");
    }

    #[test]
    fn refuses_a_mismatched_pair() {
        let dir = scratch("mismatch");
        fs::write(dir.path().join(SIGNING_KEY_FILE), [7u8; 32]).expect("положить закрытый ключ");
        fs::write(dir.path().join(PUBLIC_KEY_FILE), [1u8; 32]).expect("положить чужой открытый");

        ensure_signing_key(dir.path()).expect_err("несогласованная пара должна отвергаться");
    }
}
