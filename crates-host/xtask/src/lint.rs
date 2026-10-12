//! `cargo xtask lint [cross]` — fmt, clippy и rustdoc с нужными фичами.

use xshell::cmd;

use crate::project::{has_bootloader, root_dir};

pub fn lint_host(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir());
    cmd!(sh, "cargo fmt --check").run()?;
    cmd!(
        sh,
        "cargo clippy --workspace --all-targets --features domain/std,domain/log,adapters/signed -- -D warnings"
    )
    .run()?;
    // Битые ссылки в документации ломали бы `cargo doc` у пользователя крейта.
    cmd!(
        sh,
        "cargo doc --no-deps --workspace --exclude host-target-tests --features domain/std,domain/log,adapters/signed"
    )
    .env("RUSTDOCFLAGS", "-D warnings")
    .run()?;
    Ok(())
}

pub fn lint_cross(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
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
