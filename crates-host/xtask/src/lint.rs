//! `cargo xtask lint [cross]` — fmt, clippy и rustdoc с нужными фичами.

use anyhow::Context;
use xshell::cmd;

use crate::project::{host_features, root_dir};

pub fn lint_host(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir());
    let features = host_features();
    cmd!(sh, "cargo fmt --check").run()?;
    cmd!(
        sh,
        "cargo clippy --workspace --all-targets --features {features} -- -D warnings"
    )
    .run()?;
    // {%- if ota == "true" %}
    // `domain` без фичи `boot` — как его собирает `app` (проверка подписи —
    // только в `boot`): иначе этот вариант нигде не собирался бы. Только
    // clippy — тесты и doc без `boot` время не стоят.
    cmd!(
        sh,
        "cargo clippy --workspace --all-targets --features domain/log -- -D warnings"
    )
    .run()?;
    // {%- endif %}
    // Битые ссылки в документации ломали бы `cargo doc` у пользователя крейта.
    cmd!(
        sh,
        "cargo doc --no-deps --workspace --exclude host-target-tests --features {features}"
    )
    .env("RUSTDOCFLAGS", "-D warnings")
    .run()?;
    Ok(())
}

pub fn lint_cross(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    ensure_test_memory_matches_app()?;
    // {%- if net_services contains "rest" or net_services contains "mqtt" or ssh_auth == "password" or ssh_host_key == "file" %}
    // clippy компилирует `app`, а тот вшивает секреты служб (`include_str!`).
    crate::secrets::ensure(&root_dir())?;
    // {%- endif %}
    let _p = sh.push_dir(root_dir().join("crates-cross"));
    cmd!(sh, "cargo fmt --check").run()?;
    // `--all-targets` — та же команда, что зовёт rust-analyzer: заодно
    // линтятся тесты на устройстве (`target-tests`, свой harness). Цели
    // unit-тестов у `bsp` нет (`test = false` в его манифесте): ей нужен крейт
    // `test`, а его для `thumbv*-none-*` не существует (E0463).
    cmd!(sh, "cargo clippy --workspace --all-targets -- -D warnings").run()?;
    // Второй проход, только release: профиль release выключает
    // `debug-assertions`/`overflow-checks`, поэтому всё, что за `debug_assert!`
    // или `#[cfg(not(debug_assertions))]` (паникёр release в
    // `app/src/panic.rs`), проверяется только здесь. `bsp` линтуется вместе с
    // `app`, чтобы получить фичи `app`. В `target-tests` такого кода нет —
    // повторный проход над ними повторил бы первый.
    // {%- if ota == "true" %}
    cmd!(
        sh,
        "cargo clippy -p app -p boot -p bsp --release -- -D warnings"
    )
    .run()?;
    // Bootloader под тесты на плате (`cargo xtask test target`) собирается с
    // фичей `no-watchdog`: эта ветка нигде больше не компилируется.
    cmd!(
        sh,
        "cargo clippy -p boot --release --features no-watchdog -- -D warnings"
    )
    .run()?;
    // {%- else %}
    //{{newline}}    cmd!(sh, "cargo clippy -p app -p bsp --release -- -D warnings").run()?;
    // {%- endif %}
    Ok(())
}

/// Тесты на устройстве прошиваются на место приложения, поэтому их `memory.x`
/// обязан совпадать с `memory.x` приложения; разъехавшись, они линкуют тест не
// {%- if ota == "true" %}
/// туда, куда смотрит bootloader.
// {%- else %}
//{{newline}}/// туда, откуда стартует ядро.
// {%- endif %}
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
         файлы сверяются целиком, вместе с комментариями: скопируйте \
         crates-cross/app/memory.x поверх crates-cross/target-tests/memory.x \
         (тесты на устройстве прошиваются на место приложения)",
    );
    Ok(())
}
