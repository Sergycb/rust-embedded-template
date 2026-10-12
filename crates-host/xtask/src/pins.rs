//! `cargo xtask pins` — справочник по чипу из `crates-host/chip-info`.

use xshell::cmd;

use crate::project::root_dir;

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
pub fn pins(sh: &xshell::Shell, args: &[&str]) -> Result<(), anyhow::Error> {
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
