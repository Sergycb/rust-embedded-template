//! Ответы, данные при генерации, и пути проекта — общее для всех команд.

use std::{env, path::Path, path::PathBuf};

/// Чип, выбранный при генерации, — для `probe-rs --chip`.
pub const CHIP: &str = "{{chip}}";
/// Целевой triple прошивки — тот же, что в `crates-cross/.cargo/config.toml`. Нужен,
/// чтобы `cargo xtask setup` поставил ровно тот target, под который собирается
/// проект.
pub const TARGET: &str = "{{target}}";

/// Минимальная единица записи во flash этого чипа. Подставляется при
/// генерации, как и всё остальное про раскладку. Нужна host-target тесту:
/// состояние bootloader'а — это `WRITE_SIZE` байт одинаковой магии, короче
/// записать нельзя.
pub const WRITE_SIZE: &str = "{{write_size}}";

/// Размер страницы стирания в терминах `embassy-boot` (максимальный сектор
/// чипа). Ею bootloader переносит разделы, и на неё же смещается прежний
/// образ при обмене — host-target тесту нужно знать, где его искать.
pub const PAGE_SIZE: &str = "{{page_size}}";

/// Подписываются ли OTA-образы — ответ, данный при генерации.
pub const SIGNED: &str = "{{signed}}";

/// Имя файла ELF приложения в каталоге сборки — то, что [`crate::firmware::build`] читает.
pub const APP_ARTIFACT: &str = "app";

/// Whether this project has a bootloader at all — substituted at generation
/// time. `"false"` when the OTA layout does not fit the chip's flash: then
/// `crates-cross/boot` is not part of the generated project (see `chip-select.rhai`),
/// and the app is flashed straight to the start of flash.
///
/// A string rather than a `bool` literal on purpose: the template source has
/// to stay parseable Rust (`xtask` is a member of the root workspace and is
/// linted there), and a template placeholder only survives that inside a
/// string literal.
pub const OTA: &str = "{{ota}}";

/// Compared against `"false"` rather than `"true"` so that the un-rendered
/// template (where `OTA` is still the literal placeholder) behaves like a
/// project *with* a bootloader — that is what the maintainer expects from
/// every command that touches `crates-cross`. Note that
/// `cargo xtask lint cross` in the template repo itself cannot be that
/// command: `crates-cross/Cargo.toml` contains Liquid, so cargo refuses to
/// parse the manifest there. It is `template-check` that renders a real
/// project and lints its `crates-cross` (same reasoning as in [`crate::pins`]).
/// Generated projects always get an exact `"true"`/`"false"`.
pub fn has_bootloader() -> bool {
    OTA != "false"
}

/// Сравнение с `"true"`, а не с `"false"`: в неподставленном шаблоне (где
/// константа — сам плейсхолдер) команды должны отказывать, а не делать вид,
/// что подпись включена.
pub fn signing_enabled() -> bool {
    SIGNED == "true"
}

/// Каталог сборки `cross`-воркспейса.
///
/// Не просто `crates-cross/target`: `CARGO_TARGET_DIR` уводит артефакты в
/// другое место, и это не экзотика — так собирает `template-check`, деля один
/// кеш между конфигурациями, и так устроены CI с общим кешем. Строя путь
/// жёстко, `build` не находил бы ELF ровно там, где сборка идёт чаще всего.
pub fn cross_target_dir() -> PathBuf {
    let cross = root_dir().join("crates-cross");
    match env::var_os("CARGO_TARGET_DIR") {
        // Относительный путь cargo считает от СВОЕГО рабочего каталога, а
        // `cargo build` в `build` запускается из `crates-cross`. Разрешая его от
        // корня, мы искали бы ELF на уровень выше, чем он лёг, — причём уже
        // после успешной компиляции, что выглядит особенно загадочно.
        Some(dir) if Path::new(&dir).is_relative() => cross.join(dir),
        Some(dir) => PathBuf::from(dir),
        None => cross.join("target"),
    }
}

/// Корень проекта: два уровня вверх от манифеста, потому что сам `xtask`
/// лежит в `crates-host/`. Ошибиться тут легко и незаметно — все команды
/// строят пути от этого значения, и на уровень выше оно молча указывало бы в
/// `crates-host/`.
pub fn root_dir() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir.pop();
    dir
}
