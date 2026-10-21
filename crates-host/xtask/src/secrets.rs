//! Секреты сетевых служб, которые прошивка берёт из корня проекта: токен
//! REST API.
//!
//! Создаются случайными при первой сборке и коммитятся — как открытый ключ
//! OTA, а не как закрытый: они уезжают в образ, и кто получил `app.bin`,
//! получил и их. `.gitignore` не защитил бы ничего, а свежий клон и CI без
//! файла не собрали бы прошивку. Защита — уровня стенда; для большего у SSH
//! есть вход по ключу.

use std::fmt::Write as _;
use std::{fs, path::Path};

use anyhow::Context;

use crate::project::has_net_service;

/// Токен `Authorization: Bearer` REST API (`domain::http`), 32 шестнадцатеричных
/// знака; его вшивает `crates-cross/app/src/net.rs`.
pub const REST_TOKEN_FILE: &str = "rest-token.txt";

/// Создаёт недостающие секреты выбранных служб. Существующие не трогает:
/// новый токен отрезал бы клиентов, которые знают прежний.
pub fn ensure(root: &Path) -> Result<(), anyhow::Error> {
    if has_net_service("rest") {
        ensure_token(&root.join(REST_TOKEN_FILE))?;
    }
    Ok(())
}

fn ensure_token(path: &Path) -> Result<(), anyhow::Error> {
    if path.exists() {
        return Ok(());
    }
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes).context("не удалось получить случайные байты от ОС")?;
    let mut token = String::new();
    for b in bytes {
        write!(token, "{b:02x}").expect("запись в String не отказывает");
    }
    fs::write(path, format!("{token}\n"))
        .with_context(|| format!("записать {}", path.display()))?;
    println!(
        "создан {} — токен REST API, вшивается в прошивку. Закоммитьте его: без файла \
         прошивка не собирается",
        path.display()
    );
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::ensure_token;
    use crate::memory::scratch::scratch;

    #[test]
    fn a_missing_token_is_created_random() {
        let dir = scratch("token-new");
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        ensure_token(&a).unwrap();
        ensure_token(&b).unwrap();
        let token = fs::read_to_string(&a).unwrap();
        let token = token.trim_end();
        assert_eq!(token.len(), 32);
        assert!(token.bytes().all(|c| c.is_ascii_hexdigit()), "{token}");
        assert_ne!(token, fs::read_to_string(&b).unwrap().trim_end());
    }

    #[test]
    fn an_existing_token_is_kept() {
        let dir = scratch("token-kept");
        let path = dir.path().join("t.txt");
        fs::write(&path, "mine\n").unwrap();
        ensure_token(&path).unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "mine\n");
    }
}
