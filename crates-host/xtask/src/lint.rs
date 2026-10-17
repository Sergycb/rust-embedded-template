//! `cargo xtask lint [cross]` — fmt, clippy и rustdoc с нужными фичами.

use anyhow::Context;
use xshell::cmd;

use crate::project::{has_bootloader, root_dir};

pub fn lint_host(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir());
    cmd!(sh, "cargo fmt --check").run()?;
    cmd!(
        sh,
        "cargo clippy --workspace --all-targets --features domain/log,domain/signed -- -D warnings"
    )
    .run()?;
    // Ветка без подписи (`signed=no`): иначе её код нигде не собирался бы.
    // Только clippy — тесты и doc без `signed` время не стоят.
    cmd!(
        sh,
        "cargo clippy --workspace --all-targets --features domain/log -- -D warnings"
    )
    .run()?;
    // Битые ссылки в документации ломали бы `cargo doc` у пользователя крейта.
    cmd!(
        sh,
        "cargo doc --no-deps --workspace --exclude host-target-tests --features domain/log,domain/signed"
    )
    .env("RUSTDOCFLAGS", "-D warnings")
    .run()?;
    Ok(())
}

/// Тесты на устройстве прошиваются на место приложения, поэтому их `memory.x`
/// обязан совпадать с `memory.x` приложения; разъехавшись, они линкуют тест не
/// туда, куда смотрит bootloader.
fn ensure_test_memory_matches_app() -> Result<(), anyhow::Error> {
    let cross = root_dir().join("crates-cross");
    let read = |crate_dir: &str| {
        let path = cross.join(crate_dir).join("memory.x");
        std::fs::read_to_string(&path)
            .map(|text| text.replace("\r\n", "\n"))
            .with_context(|| format!("прочитать {}", path.display()))
    };
    anyhow::ensure!(
        read("app")? == read("target-tests")?,
        "crates-cross/target-tests/memory.x расходится с crates-cross/app/memory.x — \
         файлы обязаны совпадать (их пишет chip-select.rhai одним текстом)",
    );
    Ok(())
}

pub fn lint_cross(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    ensure_test_memory_matches_app()?;
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
    // transport pattern in graph.rs) is only type-checked here.
    // bsp/target-tests carry no such code, so re-linting them would just repeat
    // the first pass.
    if has_bootloader() {
        cmd!(sh, "cargo clippy -p app -p boot --release -- -D warnings").run()?;
    } else {
        cmd!(sh, "cargo clippy -p app --release -- -D warnings").run()?;
    }
    Ok(())
}
