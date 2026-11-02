//! `cargo xtask test <этап>` — три этапа тестирования и их сумма.

use std::env;
// {%- if ota == "true" and has_protobuf == "true" %}
use std::{
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
// {%- endif %}
use xshell::cmd;

use crate::flash::{check_app, flash_all, flash_images};
// {%- if ota == "true" %}
use crate::flash::flash_boot;
// {%- endif %}
// {%- if ota == "true" and has_protobuf == "true" %}
use crate::firmware::{OtaImage, ota_image};
use crate::image::{raw_image, unpack_version};
use crate::memory::{app_memory_regions, app_region};
use crate::project::{APP_ARTIFACT, TARGET, cross_target_dir};
use crate::signing::{ensure_image_key, ensure_signing_key, foreign_signature, signature};
// {%- endif %}
use crate::project::{CHIP, host_features, root_dir};

/// Этапы: имя, строка справки, запуск. Одна таблица и для справки, и для
/// выбора — разъехаться им негде.
const STAGES: [(&str, &str, Stage); 4] = [
    ("host", "логика domain на хосте — плата не нужна", test_host),
    (
        "target",
        "тесты внутри МК (embedded-test + probe-rs run) — нужна плата с пробником",
        test_target,
    ),
    (
        "host-target",
        "хост проверяет готовое устройство по протоколу проекта — нужна плата на связи",
        test_host_target,
    ),
    ("all", "все три подряд", test_all),
];

type Stage = fn(&xshell::Shell) -> Result<(), anyhow::Error>;
// {%- if ota == "true" and has_protobuf == "true" %}

/// Временный каталог образов OTA для сценариев: удаляется, когда
/// `test host-target` кончается, — и после ошибки, и при панике (`Drop`).
///
/// Тестовый образ подписан ключом проекта (иначе устройство с этим ключом
/// его не примет), и пережить прогон он не должен: ни в `target/`, ни рядом с
/// `app.bin` его нет.
struct Scratch(PathBuf);

impl Scratch {
    /// Новый каталог в `dir` со случайным именем (16 байт `getrandom`): имя
    /// не угадать заранее, а существующий каталог — ошибка, а не повод его
    /// взять ([`create_private_dir`]) — подложить каталог или симлинк и
    /// перехватить подписанный образ нельзя.
    fn new_in(dir: &Path) -> Result<Self, anyhow::Error> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).context("не удалось получить случайные байты от ОС")?;
        let mut name = String::from("host-target-");
        for byte in random {
            write!(name, "{byte:02x}").expect("запись в String не отказывает");
        }
        let path = dir.join(name);
        create_private_dir(&path).with_context(|| format!("создать {}", path.display()))?;
        Ok(Self(path))
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // Не удалось — каталог во временных файлах ОС; ронять из-за этого
        // нечего, а паника в `Drop` при раскрутке оборвала бы процесс.
        fs::remove_dir_all(&self.0).ok();
    }
}
// {%- endif %}

/// Без этапа — список этапов, а не запуск какого-то из них, и код выхода
/// ненулевой: иначе `cargo xtask test` в скрипте давал бы зелёный прогон, в
/// котором не выполнилось ни одного теста.
pub fn test(sh: &xshell::Shell, stage: &[&str]) -> Result<(), anyhow::Error> {
    if let [name] = stage
        && let Some((_, _, run)) = STAGES.iter().find(|(known, ..)| known == name)
    {
        return run(sh);
    }
    println!("USAGE cargo xtask test <ЭТАП>");
    for (name, about, _) in STAGES {
        println!("      {name:<12} # {about}");
    }
    match stage {
        [] => anyhow::bail!("не указан этап тестирования"),
        other => anyhow::bail!("неизвестный этап: cargo xtask test {}", other.join(" ")),
    }
}

/// Все три этапа, и каждый образ шьётся по разу: рабочую прошивку после
/// тестов внутри МК здесь не заливает `test target`, а первым делом
/// `test host-target` — заливает и после себя на плате оставляет. Упали тесты
/// внутри МК — рабочая прошивка шьётся сразу, как в отдельном этапе.
///
/// Без пробника этап внутри МК пропускается с предупреждением (код выхода —
/// нулевой): `host-target` проверяет готовое устройство по протоколу, и
/// пробник ему не нужен. Отдельный `test target` без пробника — ошибка.
fn test_all(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    test_host(sh)?;
    if probe_present(sh) {
        if let Err(failed) = run_target_tests(sh) {
            flash_all(sh, "release")?;
            return Err(failed);
        }
    } else {
        println!("ВНИМАНИЕ: пробника нет — этап target (тесты внутри МК) пропущен");
    }
    test_host_target(sh)
}

fn test_host(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir());
    let features = host_features();
    cmd!(
        sh,
        "cargo nextest run --workspace --exclude host-target-tests --release --features {features}"
    )
    .run()?;
    // nextest doc-тесты не запускает.
    cmd!(
        sh,
        "cargo test --doc --workspace --exclude host-target-tests --release --features {features}"
    )
    .run()?;
    Ok(())
}

/// Хост проверяет готовое устройство только через транспорт и протокол
/// проекта — так, как с ним будет говорить настоящий хост.
///
/// Пробник здесь — лишь необязательная прошивка перед тестами: есть — шьём
/// release-образы; нет — тесты идут против того, что уже прошито.
// {%- if ota == "true" and has_protobuf == "true" %}
/// С OTA тестам передаётся и ожидаемая версия.
// {%- endif %}
/// Перепрошивки после тестов нет: сценарии оставляют устройство на рабочем
/// образе. Сверка начала образа с разделом из `memory.x` — всегда: это
/// проверка сборки, а не железа.
///
// {%- if has_protobuf == "true" %}
/// Адрес устройства — из окружения (`HOST_TARGET_ADDR` или `HOST_TARGET_PORT`,
/// README, «Трёхэтапное тестирование»); без него сценарии пропускаются, и
/// сводка после прогона говорит почему.
// {%- else %}
/// Сценарии написаны под protobuf: под протокол проекта их нет, и прогон
/// пустой — причину этап печатает сам.
// {%- endif %}
fn test_host_target(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    // {%- if ota == "true" and has_protobuf == "true" %}
    // Ключ — до сборки, как и в `build`: открытый попадает в bootloader через
    // его build.rs, то есть должен существовать к моменту компиляции.
    let seed = ensure_signing_key(&root_dir())?;
    // {%- endif %}
    let flash = probe_present(sh);
    check_app(sh, "release")?;
    // {%- if ota == "true" and has_protobuf == "true" %}
    let addressed = !(unset("HOST_TARGET_ADDR") && unset("HOST_TARGET_PORT"));
    // Образы для сценариев OTA — до прошивки: сборка, которая не сходится
    // (раскладка, ключ), не должна оставить плату с новым bootloader'ом. Без
    // адреса сценарии не пойдут — и собирать их незачем.
    let _ota_env = if addressed {
        Some(ota_env(sh, seed.as_ref(), flash)?)
    } else {
        None
    };
    // {%- endif %}
    if flash {
        flash_images(sh, "release")?;
    } else {
        println!("пробника нет — прошивка пропущена, тесты идут против уже прошитого устройства");
    }

    let _env_chip = sh.push_env("HOST_TARGET_CHIP", CHIP);
    let _p = sh.push_dir(root_dir().join("crates-host").join("host-target-tests"));
    // {%- if has_protobuf == "true" %}
    // По одному: канал к устройству один, а nextest по умолчанию гоняет тесты
    // параллельно — два сценария разом мешали бы друг другу (чужой `Start`
    // получает `Busy`, `Reboot` одного рвёт связь другому).
    //
    // Пропуски сценарии пишут в журнал (`host_target_tests::skip`), а сводка
    // по нему — и после упавшего прогона: в выводе nextest пропуск не виден ни
    // при каком исходе.
    let log = env::temp_dir().join(format!("host-target-skips-{}.log", std::process::id()));
    // Журнал от прошлого прогона с тем же номером процесса сосчитался бы дважды.
    std::fs::remove_file(&log).ok();
    let run = {
        let _log = sh.push_env("HOST_TARGET_SKIP_LOG", &log);
        cmd!(sh, "cargo nextest run --test-threads 1").run()
    };
    report_skipped(&std::fs::read_to_string(&log).unwrap_or_default());
    std::fs::remove_file(&log).ok();
    run?;
    // {%- else %}
    // Сценарии написаны под protobuf; под протокол проекта их нет —
    // пустой прогон не ошибка, а причина печатается.
    //{{newline}}    let why = "{% if has_modbus == "true" %}Modbus — следующая итерация шаблона{% elsif has_canopen == "true" %}CANopen — отложены{% else %}свой протокол — тесты под него пишет проект{% endif %}";
    //{{newline}}    println!("сценариев host-target нет: {why} (crates-host/host-target-tests)");
    //{{newline}}    cmd!(sh, "cargo nextest run --test-threads 1 --no-tests warn").run()?;
    // {%- endif %}
    Ok(())
}

/// Есть ли пробник — от этого зависит только прошивка перед `host-target` и
/// этап `target` внутри `test all`.
///
/// Задан `PROBE_RS_PROBE` — пробник назван явно, и он считается
/// подключённым: не найдётся — прошивка упадёт с ошибкой, а не тесты молча
/// пойдут против чужой прошивки. Иначе — `probe-rs list`; не запустился он
/// сам (не установлен) — пробника тоже нет.
fn probe_present(sh: &xshell::Shell) -> bool {
    if !unset("PROBE_RS_PROBE") {
        return true;
    }
    match cmd!(sh, "probe-rs list").quiet().ignore_stderr().read() {
        Ok(list) => lists_a_probe(&list),
        Err(err) => {
            println!("probe-rs list не запустился ({err}) — считаем, что пробника нет");
            false
        }
    }
}

/// Переменная окружения не задана или пустая — так CI отдаёт переменную
/// репозитория, которой нет.
fn unset(name: &str) -> bool {
    env::var_os(name).is_none_or(|value| value.is_empty())
}

/// В выводе `probe-rs list` есть пробник: строка вида `[0]: STLink V3 -- …`.
fn lists_a_probe(list: &str) -> bool {
    list.lines().any(|line| {
        line.trim_start()
            .strip_prefix('[')
            .and_then(|rest| rest.split_once("]:"))
            .is_some_and(|(index, _)| {
                !index.is_empty() && index.bytes().all(|b| b.is_ascii_digit())
            })
    })
}
// {%- if has_protobuf == "true" %}

/// Итог после прогона: nextest считает пропуск внутри теста (`return` без
/// окружения) пройденным тестом, и зелёная сводка выглядела бы полной
/// проверкой. `log` — журнал пропусков, строка на пропущенный сценарий с его
/// причиной; одинаковые причины складываются.
fn report_skipped(log: &str) {
    let mut reasons: Vec<(usize, &str)> = Vec::new();
    for why in log.lines().filter(|line| !line.is_empty()) {
        match reasons.iter_mut().find(|(_, known)| *known == why) {
            Some((count, _)) => *count += 1,
            None => reasons.push((1, why)),
        }
    }
    let total: usize = reasons.iter().map(|(count, _)| count).sum();
    if total == 0 {
        return;
    }
    println!("ВНИМАНИЕ: тестов host-target, прошедших пропуском, ничего не проверив, — {total}:");
    for (count, why) in reasons {
        println!("  {count} — {why}");
    }
}
// {%- endif %}
// {%- if ota == "true" and has_protobuf == "true" %}

/// Образы для сценариев OTA — в окружение теста.
///
/// Тестовый образ `app-unconfirmed` (версия рабочего + 1, себя не
/// подтверждает) — с подписью ключом проекта, если он есть, и подписью
/// случайным ключом, которого нет ни в одном bootloader'е. Рабочий образ —
/// для отказа той же версии. Длина `ACTIVE` — сроки ожидания стирания и
/// обмена. Ожидаемая версия — только если плату прошили здесь: иначе на ней
/// неизвестно что.
///
/// Образы и подписи — только во временном каталоге ([`Scratch`]), который
/// живёт, пока жив результат, как и переменные (guard'ы `xshell`).
fn ota_env<'a>(
    sh: &'a xshell::Shell,
    seed: Option<&[u8; 32]>,
    flash: bool,
) -> Result<(Scratch, Vec<xshell::PushEnv<'a>>), anyhow::Error> {
    {
        let _p = sh.push_dir(root_dir().join("crates-cross"));
        // Bootloader — отдельным вызовом, как в `build` (там же почему): с
        // его ELF сверяются раскладка и ключ.
        cmd!(sh, "cargo build -p boot --release").run()?;
        cmd!(sh, "cargo build -p app --release --bin app-unconfirmed").run()?;
    }
    let release = cross_target_dir().join(TARGET).join("release");
    let working = ota_image(&release.join(APP_ARTIFACT))?;
    let unconfirmed = ota_image(&release.join("app-unconfirmed"))?;
    anyhow::ensure!(
        Some(unconfirmed.version) == working.version.checked_add(1),
        "тестовый образ app-unconfirmed собран с версией {:#010x}, а не рабочей + 1 \
         ({:#010x}) — сверьте `IMAGE` в crates-cross/app/src/bin/app-unconfirmed.rs и \
         crates-cross/app/src/main.rs",
        unconfirmed.version,
        working.version,
    );
    let regions = app_memory_regions()?;
    let (_, active) = app_region(&regions).context("в memory.x приложения нет раздела ACTIVE")?;

    let dir = Scratch::new_in(&env::temp_dir())?;
    let write = |name: &str, bytes: &[u8]| {
        let path = dir.0.join(name);
        fs::write(&path, bytes)
            .with_context(|| format!("записать {}", path.display()))
            .map(|()| path)
    };
    let OtaImage { image, .. } = &unconfirmed;
    let mut vars = vec![
        sh.push_env("HOST_TARGET_IMAGE", write("app.bin", &working.image)?),
        sh.push_env(
            "HOST_TARGET_UNCONFIRMED_IMAGE",
            write("app-unconfirmed.bin", image)?,
        ),
        sh.push_env(
            "HOST_TARGET_FOREIGN_SIG",
            write(
                "app-unconfirmed.bin.foreign-sig",
                &foreign_signature(image)?,
            )?,
        ),
        // Сколько ждать стирания `DFU` и обмена разделов — от размера `ACTIVE`.
        sh.push_env("HOST_TARGET_ACTIVE_LEN", active.length.to_string()),
    ];
    if let Some(seed) = seed {
        // Как в `build`: ключ в bootloader'е обязан быть тем, которым
        // подписывается образ, — иначе тест обмена упал бы, обвиняя
        // bootloader, а не сборку в старом ключе. Сверяется с собранным здесь
        // ELF; в чужом, уже прошитом bootloader'е (без пробника) ключ не
        // проверить — тест отката называет эту причину сам.
        let boot_elf = release.join("boot");
        let (boot_base, boot_image) = raw_image(&boot_elf)?;
        ensure_image_key(&boot_elf, boot_base, &boot_image, seed)?;
        vars.push(sh.push_env(
            "HOST_TARGET_UNCONFIRMED_SIG",
            write("app-unconfirmed.bin.sig", &signature(image, seed))?,
        ));
    }
    if flash {
        let (major, minor, patch) = unpack_version(working.version);
        vars.push(sh.push_env(
            "HOST_TARGET_EXPECT_VERSION",
            format!("{major}.{minor}.{patch}"),
        ));
    }
    Ok((dir, vars))
}

/// Создаёт каталог `path`, которого ещё нет: `create_dir`, а не `_all` —
/// существующий (подложенный заранее, симлинк) — ошибка. На unix права 0700
/// ставятся атомарно при создании; на Windows `%TEMP%` и так у каждого
/// пользователя свой.
#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    fs::DirBuilder::new().mode(0o700).create(path)
}

/// См. unix-вариант выше: здесь права не нужны — `%TEMP%` свой у каждого.
#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)
}
// {%- endif %}

/// Тесты внутри МК, затем рабочая прошивка (`flash_all`, release) — и после
/// упавших тестов тоже. Без неё на плате остался бы тестовый образ{% if ota == "true" %} над
/// bootloader'ом без сторожа (`no-watchdog`) — и молча пропала бы защита
/// «образ завис до `spawn_all` — сторож сбросил, bootloader откатил»{% endif %}.
fn test_target(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let tests = run_target_tests(sh);
    flash_all(sh, "release")?;
    tests
}

// {%- if ota == "true" %}
/// Сами тесты внутри МК, без рабочей прошивки после них. Bootloader
/// заливается первым: тестовый образ линкуется в `ACTIVE` (его `memory.x` —
/// копия app'ового), а `probe-rs run` после заливки сбрасывает чип, и
/// управление получает не тест, а то, что лежит с базы flash. На плате, где
/// bootloader'а нет — свежей или после `probe-rs erase`, — без этой строки не
/// выполнился бы ни один тест.
// {%- else %}
//{{newline}}/// Сами тесты внутри МК, без рабочей прошивки после них.
// {%- endif %}
fn run_target_tests(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    // {%- if ota == "true" %}
    flash_boot(sh, false)?;
    // {%- endif %}
    let _p = sh.push_dir(root_dir().join("crates-cross/target-tests"));
    cmd!(sh, "cargo test").run()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    /// Пробник в выводе `probe-rs list` — строка с номером в скобках; без
    /// пробников — ни одной такой.
    #[test]
    fn a_probe_is_a_numbered_line_of_probe_rs_list() {
        assert!(super::lists_a_probe(
            "The following debug probes were found:\n[0]: STLink V3 -- 0483:374e:002A (ST-LINK)\n"
        ));
        assert!(!super::lists_a_probe("No debug probes were found.\n"));
        assert!(!super::lists_a_probe("[x]: не номер\n[]: пусто\n"));
    }
    // {%- if ota == "true" and has_protobuf == "true" %}

    /// Каталог образов: имя случайное, существующий не берётся, и после
    /// `Drop` его нет.
    #[test]
    fn the_image_scratch_is_fresh_and_goes_away() {
        let base = crate::memory::scratch::scratch("host-target-scratch");
        let first = super::Scratch::new_in(base.path()).expect("каталог");
        let second = super::Scratch::new_in(base.path()).expect("каталог");
        assert_ne!(first.0, second.0, "имя случайное");
        assert!(first.0.is_dir());
        super::create_private_dir(&first.0).expect_err("существующий каталог не берётся");

        let path = first.0.clone();
        drop(first);
        assert!(!path.exists(), "каталог удалён в Drop");
    }
    // {%- endif %}
}
