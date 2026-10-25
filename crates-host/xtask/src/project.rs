//! Ответы, данные при генерации, и пути проекта — общее для всех команд.

use std::{env, path::Path, path::PathBuf};

/// Чип, выбранный при генерации, — для `probe-rs --chip`.
pub const CHIP: &str = "{{chip}}";
/// Целевой triple прошивки — тот же, что в `crates-cross/.cargo/config.toml`. Нужен,
/// чтобы `cargo xtask setup` поставил ровно тот target, под который собирается
/// проект.
pub const TARGET: &str = "{{target}}";

// {%- if ota == "true" and ota_store != "external" %}
/// Минимальная единица записи во flash этого чипа. Подставляется при
/// генерации, как и всё остальное про раскладку. Нужна host-target тесту:
/// состояние bootloader'а — это `WRITE_SIZE` байт одинаковой магии, короче
/// записать нельзя.
pub const WRITE_SIZE: &str = "{{write_size}}";

/// Страница обмена `embassy-boot` (`PAGE_SIZE`): максимум из страниц стирания
/// `ACTIVE` и `DFU`. Бывает крупнее физической страницы чипа — на чипах с
/// мелкими страницами генерация расширяет её (`swap_paged`), чтобы журнал
/// `BOOTLOADER_STATE` не отнимал место у кода bootloader'а. Ею bootloader
/// переносит разделы, и на неё же смещается прежний образ при обмене —
/// host-target тесту нужно знать, где его искать.
pub const PAGE_SIZE: &str = "{{page_size}}";
// {%- endif %}
// {%- if ota == "true" %}

/// Подписываются ли OTA-образы — ответ, данный при генерации. Строкой:
/// исходник шаблона обязан оставаться разбираемым Rust (`xtask` — член
/// корневого workspace и линтуется там), а плейсхолдер переживает это только
/// внутри строкового литерала.
pub const SIGNED: &str = "{{signed}}";
// {%- endif %}

/// Имя файла ELF приложения в каталоге сборки — то, что [`crate::firmware::build`] читает.
pub const APP_ARTIFACT: &str = "app";

/// Фичи `domain` для проверок на хосте.
// {%- if ota == "true" %}
/// С подписью образа (`domain/signed`): без неё её код нигде не проверялся бы.
// {%- endif %}
pub fn host_features() -> &'static str {
    // {%- if ota == "true" %}
    "domain/log,domain/signed"
    // {%- else %}
    //{{newline}}    "domain/log"
    // {%- endif %}
}
// {%- if ota == "true" %}

/// Сравнение с `"true"`, а не с `"false"`: в неподставленном шаблоне (где
/// константа — сам плейсхолдер) команды должны отказывать, а не делать вид,
/// что подпись включена.
pub fn signing_enabled() -> bool {
    SIGNED == "true"
}
// {%- endif %}

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
