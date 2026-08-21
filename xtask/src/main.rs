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
        // Аргументы уходят в chip-info как есть (блок, вывод, `--snippet`,
        // `--check`) — разбирает их он, здесь незачем знать его ключи.
        ["pins", args @ ..] => pins(&sh, args),
        ["panic"] => panic_dump(&sh),
        ["ota-key"] => ota_key(),
        ["ota-sign", image] => ota_sign(image),
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
    println!("      cargo xtask pins [БЛОК|ПИН]            # справочник по чипу: SPI1, PA9");
    println!(
        "      cargo xtask pins БЛОК --snippet        # заготовка bind_interrupts!/assign_resources!"
    );
    println!(
        "      cargo xtask pins --check               # не занят ли отладочный порт в resources.rs"
    );
    println!("      cargo xtask panic                      # причина последней паники с платы");
    if signing_enabled() {
        println!("      cargo xtask ota-key                    # создать ключ подписи образов");
        println!("      cargo xtask ota-sign <образ.bin>       # подписать образ для OTA");
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

/// Чип-фича `embassy-stm32`/`stm32-metapac`, выбранная при генерации. Нужна
/// не для запуска, а чтобы отличить сгенерированный проект от самого
/// репозитория шаблона — см. [`pins`].
const CHIP_FEATURE: &str = "{{chip_feature}}";

/// Справочник по чипу: `cargo xtask pins SPI1`, `cargo xtask pins PA9`, без
/// аргумента — список блоков с выводами.
///
/// Обёртка здесь оправдана (в отличие от убранных обёрток над `probe-rs`):
/// путь к манифесту `chip-info` относительный, и прямой вызов `cargo run
/// --manifest-path chip-info/Cargo.toml` работал бы только из корня проекта.
///
/// Отдельный крейт, а не подкоманда прямо здесь, — вынужденно: `stm32-metapac`
/// выбирает чип Cargo-фичей, то есть в его манифесте живёт Liquid, а манифест
/// с Liquid не резолвится. Будь он членом корневого workspace, в репозитории
/// шаблона перестали бы работать `cargo xtask lint` и `test host`. Подробнее —
/// в doc-комментарии `chip-info/src/main.rs`.
fn pins(sh: &xshell::Shell, args: &[&str]) -> Result<(), anyhow::Error> {
    // В самом репозитории шаблона плейсхолдер не подставлен — там chip-info не
    // собирается вовсе, и невнятная ошибка Liquid-парсинга манифеста лучше
    // объясняется здесь.
    if CHIP_FEATURE.starts_with('{') {
        anyhow::bail!(
            "`cargo xtask pins` работает только в сгенерированном проекте: в репозитории \
             шаблона в chip-info/Cargo.toml вместо чип-фичи стоит Liquid-плейсхолдер. \
             Проверять эту команду — через `cargo run --manifest-path chip-data-gen/Cargo.toml \
             --bin template-check`"
        );
    }
    let _p = sh.push_dir(root_dir());
    // Без `--quiet`: первый запуск собирает stm32-metapac (~15 секунд), и
    // молчащий терминал выглядел бы как зависание.
    cmd!(
        sh,
        "cargo run --manifest-path chip-info/Cargo.toml -- {args...}"
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

/// Сравнение с `"true"`, а не с `"false"`: в неподставленном шаблоне (где
/// константа — сам плейсхолдер) команды должны отказывать, а не делать вид,
/// что подпись включена.
fn signing_enabled() -> bool {
    SIGNED == "true"
}

fn ensure_signing_enabled() -> Result<(), anyhow::Error> {
    anyhow::ensure!(
        signing_enabled(),
        "подпись образов в этом проекте не включена: она выбирается при генерации \
         (`--define signed=yes`) и требует OTA. Включать её в существующем проекте — \
         это фича `ed25519-salty` у embassy-boot в cross/bsp/Cargo.toml, ключ в \
         cross/bsp/src/ota.rs и вызов verify_and_mark_updated вместо mark_updated",
    );
    Ok(())
}

/// Создаёт ключевую пару и печатает открытый ключ в виде, готовом к вставке в
/// `cross/bsp/src/ota.rs`.
///
/// Закрытый ключ пишется в файл и больше нигде не появляется — ни в логе, ни
/// в прошивке. Потерять его значит потерять возможность выпускать обновления
/// для уже прошитых устройств: открытый ключ зашит в их образ, и подпись
/// другим ключом они не примут.
fn ota_key() -> Result<(), anyhow::Error> {
    ensure_signing_enabled()?;
    let path = root_dir().join(SIGNING_KEY_FILE);
    anyhow::ensure!(
        !path.exists(),
        "ключ уже есть: {}. Новый сделает бесполезными все устройства, прошитые со \
         старым открытым ключом, — если это правда нужно, удалите файл вручную",
        path.display(),
    );

    let mut seed = [0u8; 32];
    getrandom::fill(&mut seed).context("не удалось получить случайные байты от ОС")?;
    fs::write(&path, seed).with_context(|| format!("записать {}", path.display()))?;

    let public = salty::Keypair::from(&seed).public.to_bytes();
    println!(
        "закрытый ключ: {} (в .gitignore, храните отдельно)",
        path.display()
    );
    println!();
    println!("вставьте в cross/bsp/src/ota.rs:");
    println!("pub const PUBLIC_KEY: [u8; 32] = [");
    for chunk in public.chunks(8) {
        let row = chunk
            .iter()
            .map(|byte| format!("0x{byte:02X},"))
            .collect::<Vec<_>>()
            .join(" ");
        println!("    {row}");
    }
    println!("];");
    Ok(())
}

/// Подписывает готовый образ: считает его SHA-512 и подписывает сам хеш —
/// именно так проверяет `embassy-boot` (`verify_and_mark_updated` хеширует
/// раздел `DFU` и проверяет подпись хеша, а не образа).
///
/// На вход идёт СЫРОЙ образ, тот же, что уедет в `DFU`, а не ELF из
/// `cross/target/...`: ELF содержит секции и символы, которых во flash нет.
/// Получить сырой можно `cargo objcopy` (llvm-tools) или
/// `probe-rs read`-независимыми средствами вроде `arm-none-eabi-objcopy -O
/// binary`.
fn ota_sign(image: &str) -> Result<(), anyhow::Error> {
    ensure_signing_enabled()?;
    let key_path = root_dir().join(SIGNING_KEY_FILE);
    let seed = fs::read(&key_path).with_context(|| {
        format!(
            "нет ключа {} — создайте его: cargo xtask ota-key",
            key_path.display()
        )
    })?;
    let seed: [u8; 32] = seed
        .try_into()
        .map_err(|_| anyhow::anyhow!("{} должен быть ровно 32 байта", key_path.display()))?;

    let bytes = fs::read(image).with_context(|| format!("прочитать образ {image}"))?;
    anyhow::ensure!(!bytes.is_empty(), "образ {image} пуст");
    if bytes.starts_with(b"\x7fELF") {
        anyhow::bail!(
            "{image} — это ELF, а не сырой образ. Во flash уезжают только байты секций: \
             сделайте bin (`cargo objcopy --release -- -O binary app.bin`) и подпишите его"
        );
    }

    let digest = salty::Sha512::new().updated(&bytes).finalize();
    let signature = salty::Keypair::from(&seed).sign(&digest).to_bytes();
    let signature_path = format!("{image}.sig");
    fs::write(&signature_path, signature).with_context(|| format!("записать {signature_path}"))?;

    println!("подписан {image}: {} байт", bytes.len());
    println!("подпись: {signature_path} (64 байта)");
    println!();
    println!(
        "устройству нужны обе величины: подпись и длина образа ({}) — их принимает \
         Ota::verify_and_mark_updated",
        bytes.len(),
    );
    Ok(())
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
    let memory_x = root_dir().join("cross").join("app").join("memory.x");
    let regions = parse_memory_regions(&memory_x)
        .with_context(|| format!("разобрать {}", memory_x.display()))?;
    let panic = region(&regions, "PANIC").context(
        "в cross/app/memory.x нет региона PANIC — на этом чипе дамп паники негде хранить",
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
fn root_dir() -> PathBuf {
    let mut xtask_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    xtask_dir.pop();
    xtask_dir
}
