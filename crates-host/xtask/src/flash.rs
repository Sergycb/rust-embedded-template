//! Прошивка платы перед этапами `test target` и `test host-target`.
//!
//! Отдельной команды `flash` нет: руками прошивают `cargo flash -p boot` и
//! `cargo run -p app` в `crates-cross` (см. README, «Быстрый старт»). Здесь
//! только то, что тестам нужно залить самим, и в правильном порядке —
//! сначала bootloader.

use anyhow::Context;
use xshell::cmd;

use crate::image::{ensure_image_base, raw_image};
use crate::memory::{app_memory_regions, app_region};
use crate::project::{APP_ARTIFACT, CHIP, TARGET, cross_target_dir, has_bootloader, root_dir};

/// Bootloader и приложение. `profile` — `"debug"` или `"release"`, он же имя
/// каталога сборки.
pub fn flash_all(sh: &xshell::Shell, profile: &str) -> Result<(), anyhow::Error> {
    // Сверка образа приложения — ДО прошивки bootloader'а: иначе при расхождении
    // базы плата к моменту отказа уже получила бы новый bootloader.
    //
    // Проверка раскладки вообще — до первой записи в железо, а не после:
    // прошивка необратима, и её откат стоит отдельного флеш-цикла. `cargo
    // flash` сам пересобирает пакет, но прочитать ELF получается только после
    // сборки, а `build` между ними может не стоять. Поэтому образ собираем
    // здесь и сразу сверяем его начало с началом раздела — той же проверкой,
    // что и в `build`, только без подписи и печати размера.
    check_image_origin(sh, profile)?;
    flash_boot(sh, profile)?;
    flash_package(sh, APP_ARTIFACT, profile)
}

/// Только bootloader — если он в проекте есть. Сверять его образ с разделом
/// приложения не нужно: ELF бутлоадера линкуется с базы флеша, а не в
/// `ACTIVE`, и сравнение с `ORIGIN(ACTIVE)` было бы сравнением разных адресов.
pub fn flash_boot(sh: &xshell::Shell, profile: &str) -> Result<(), anyhow::Error> {
    if !has_bootloader() {
        return Ok(());
    }
    flash_package(sh, "boot", profile)
}

fn flash_package(sh: &xshell::Shell, package: &str, profile: &str) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir().join("crates-cross"));
    match profile {
        "release" => cmd!(sh, "cargo flash -p {package} --release --chip {CHIP}").run()?,
        _ => cmd!(sh, "cargo flash -p {package} --chip {CHIP}").run()?,
    }
    Ok(())
}

/// Собирает приложение и сверяет начало его образа с началом раздела.
///
/// Лишний `cargo build` здесь — попадание в кеш: следующий `cargo flash`
/// собирает тот же пакет теми же флагами и почти ничего не делает.
fn check_image_origin(sh: &xshell::Shell, profile: &str) -> Result<(), anyhow::Error> {
    {
        let _p = sh.push_dir(root_dir().join("crates-cross"));
        if profile == "release" {
            cmd!(sh, "cargo build -p {APP_ARTIFACT} --release").run()?;
        } else {
            cmd!(sh, "cargo build -p {APP_ARTIFACT}").run()?;
        }
    }

    let elf = cross_target_dir()
        .join(TARGET)
        .join(profile)
        .join(APP_ARTIFACT);
    anyhow::ensure!(
        elf.exists(),
        "сборка прошла, но ELF по пути {} не найден — сверять нечего, а прошивать без сверки \
         опасно. Так бывает, если каталог сборки задан не переменной CARGO_TARGET_DIR, а \
         `[build] target-dir` в .cargo/config.toml — его xtask не читает",
        elf.display(),
    );
    let (base, _) = raw_image(&elf)?;

    let regions = app_memory_regions()?;
    let (name, partition) = app_region(&regions).with_context(|| {
        format!(
            "в {} нет ни региона ACTIVE, ни FLASH — адрес образа не с чем сверять",
            root_dir().join("crates-cross/app/memory.x").display(),
        )
    })?;
    ensure_image_base(base, name, partition)
}
