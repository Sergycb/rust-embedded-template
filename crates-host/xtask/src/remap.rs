//! Префиксы машины сборки — вон из образа: `crates-cross/.cargo/remap.toml`.
//!
//! Место паники (`файл:строка`) компилятор кладёт в образ строкой — его
//! сохраняет паникёр release. Пути пакетов вне каталога `crates-cross` (крейты
//! `crates-host`, кеш реестра и git в `CARGO_HOME`, исходники `core` в sysroot)
//! cargo передаёт абсолютными: без правки образ нёс бы имя пользователя и
//! каталоги машины, а один коммит у двух пользователей давал бы разный
//! `app.bin` — и разную подпись. Каталог самого проекта влияет на образ и после
//! правки, но иначе — через имена символов (docs/conventions.md,
//! «Воспроизводимый релиз»).
//!
//! `--remap-path-prefix` срезает эти префиксы, а `--remap-path-scope=macro`
//! ограничивает правку местами паник: отладочная информация ELF остаётся с
//! настоящими путями, и отладчик по-прежнему находит исходники.
//!
//! Префиксы свои у каждой машины, поэтому флаги не лежат в `config.toml`, а
//! пишутся сюда перед командами `xtask` (кроме `setup`) и подключаются оттуда
//! `include` (файл в `.gitignore`). Через файл, а не `--config` у вызовов
//! `xtask`: тогда те же флаги получают и ручные `cargo run`/`cargo flash`, и их
//! сборка не расходится с `cargo xtask build`.

use std::fmt::Write as _;
use std::{
    env, fs,
    path::{Path, PathBuf},
};

use anyhow::Context;
use xshell::cmd;

use crate::project::{cross_target_dir, root_dir};

/// Записывает `remap.toml` под эту машину; не трогает файл, если он уже такой.
pub fn ensure(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let root = root_dir();
    let cargo_home = cargo_home()?;
    let _p = sh.push_dir(root.join("crates-cross"));
    // Свежий `CARGO_HOME`: каталогов индексов реестра ещё нет, и первая
    // сборка срезала бы пути зависимостей не так, как следующие, — её образ
    // разошёлся бы с ними. Скачиваем заранее то, что скачала бы она сама.
    if !fs::read_dir(cargo_home.join("registry").join("src"))
        .is_ok_and(|mut entries| entries.next().is_some())
    {
        cmd!(sh, "cargo fetch").run()?;
    }
    let sysroot = cmd!(sh, "rustc --print sysroot").quiet().read()?;
    let prefixes = prefixes(&root, &cargo_home, Path::new(&sysroot))?;

    let mut text = String::from(
        "# Создаёт `cargo xtask` под эту машину (crates-host/xtask/src/remap.rs), не коммитится.\n\
         [target.'cfg(all(target_arch = \"arm\", target_os = \"none\"))']\n\
         rustflags = [\n\t\"--remap-path-scope=macro\",\n",
    );
    for prefix in &prefixes {
        let prefix = prefix.to_str().with_context(|| {
            format!(
                "путь {} не в UTF-8 — его не записать в TOML",
                prefix.display()
            )
        })?;
        // Обычная строка TOML: экранируются обратные слэши Windows и кавычки.
        let prefix = prefix.replace('\\', "\\\\").replace('"', "\\\"");
        writeln!(text, "\t\"--remap-path-prefix={prefix}=\",")?;
    }
    text.push_str("]\n");

    let path = root.join("crates-cross").join(".cargo").join("remap.toml");
    if fs::read_to_string(&path).ok().as_deref() != Some(text.as_str()) {
        fs::write(&path, text).with_context(|| format!("записать {}", path.display()))?;
    }
    Ok(())
}

/// Префиксы, которые срезаются, — от общего к частному: при нескольких
/// подходящих rustc берёт последний.
fn prefixes(root: &Path, cargo_home: &Path, sysroot: &Path) -> Result<Vec<PathBuf>, anyhow::Error> {
    // Корень проекта: `crates-host/domain/src/lib.rs`. Каталог сборки — когда
    // `CARGO_TARGET_DIR` уводит его из корня: там код, сгенерированный build.rs.
    let mut prefixes = vec![root.to_path_buf(), cross_target_dir()];
    // Каталог индекса реестра тоже: `embassy-boot-0.7.0/src/…`, а не
    // `index.crates.io-…/embassy-boot-0.7.0/src/…`; они есть — `ensure`
    // скачивает реестр до этого.
    if let Ok(entries) = fs::read_dir(cargo_home.join("registry").join("src")) {
        for entry in entries {
            prefixes.push(entry?.path());
        }
    }
    // Git-зависимости: `rust-lib-…/8a78ea3/sync-request/src/lib.rs`.
    prefixes.push(cargo_home.join("git").join("checkouts"));
    // Исходники `core` из `rust-src`: `library/core/src/…`.
    prefixes.push(sysroot.join("lib").join("rustlib").join("src").join("rust"));
    Ok(prefixes)
}

/// `CARGO_HOME`, как его находит cargo: переменная, иначе `~/.cargo`.
fn cargo_home() -> Result<PathBuf, anyhow::Error> {
    if let Some(home) = env::var_os("CARGO_HOME") {
        return Ok(PathBuf::from(home));
    }
    env::home_dir()
        .map(|home| home.join(".cargo"))
        .context("не найден домашний каталог, а CARGO_HOME не задан")
}
