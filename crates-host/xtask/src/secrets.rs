//! Секреты сетевых служб, которые прошивка берёт из корня проекта: токен
//! REST API, пароль консоли SSH и вшиваемый ключ хоста SSH.
//!
//! Создаются случайными при первой сборке и коммитятся — как открытый ключ
//! OTA, а не как закрытый: они уезжают в образ, и кто получил `app.bin`,
//! получил и их. `.gitignore` не защитил бы ничего, а свежий клон и CI без
//! файла не собрали бы прошивку. Защита — уровня стенда; для большего у SSH
//! есть вход по ключу и ключ хоста, созданный самой платой (`config`).

use std::fmt::Write as _;
use std::{fs, path::Path};

use anyhow::Context;

use crate::project::{SSH_AUTH, SSH_HOST_KEY, has_net_service};

/// Токен `Authorization: Bearer` REST API (`domain::http`), 32 шестнадцатеричных
/// знака; его вшивает `crates-cross/app/src/net.rs`.
pub const REST_TOKEN_FILE: &str = "rest-token.txt";

/// Пароль пользователя консоли SSH (`domain::ssh::USER`) при входе по паролю,
/// 32 шестнадцатеричных знака; вшивает `crates-cross/app/src/net.rs`.
pub const SSH_PASSWORD_FILE: &str = "ssh-password.txt";

/// Зерно ключа хоста SSH (ed25519, 32 байта), когда ключ вшит в прошивку, а не
/// создан платой; вшивает `crates-cross/app/src/net.rs`.
pub const SSH_HOST_KEY_FILE: &str = "ssh-host-key.bin";

/// Создаёт недостающие секреты выбранных служб. Существующие не трогает:
/// новый токен или пароль отрезал бы клиентов, которые знают прежний, а новый
/// ключ хоста — тех, у кого он в `known_hosts`.
pub fn ensure(root: &Path) -> Result<(), anyhow::Error> {
    if has_net_service("rest") {
        ensure_text(&root.join(REST_TOKEN_FILE), "токен REST API")?;
    }
    if has_net_service("ssh") && SSH_AUTH == "password" {
        ensure_text(&root.join(SSH_PASSWORD_FILE), "пароль консоли SSH")?;
    }
    if has_net_service("ssh") && SSH_HOST_KEY == "file" {
        ensure_bytes(&root.join(SSH_HOST_KEY_FILE), "ключ хоста SSH")?;
    }
    Ok(())
}

/// 16 случайных байт шестнадцатеричной строкой — токен или пароль.
fn ensure_text(path: &Path, what: &str) -> Result<(), anyhow::Error> {
    if path.exists() {
        return Ok(());
    }
    let mut text = String::new();
    for b in random::<16>()? {
        write!(text, "{b:02x}").expect("запись в String не отказывает");
    }
    fs::write(path, format!("{text}\n")).with_context(|| format!("записать {}", path.display()))?;
    created(path, what);
    Ok(())
}

/// 32 случайных байта как есть — зерно ключа.
fn ensure_bytes(path: &Path, what: &str) -> Result<(), anyhow::Error> {
    if path.exists() {
        return Ok(());
    }
    fs::write(path, random::<32>()?).with_context(|| format!("записать {}", path.display()))?;
    created(path, what);
    Ok(())
}

fn random<const N: usize>() -> Result<[u8; N], anyhow::Error> {
    let mut bytes = [0u8; N];
    getrandom::fill(&mut bytes).context("не удалось получить случайные байты от ОС")?;
    Ok(bytes)
}

fn created(path: &Path, what: &str) {
    println!(
        "создан {} — {what}, вшивается в прошивку. Закоммитьте его: без файла прошивка не \
         собирается",
        path.display()
    );
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::{ensure_bytes, ensure_text};
    use crate::memory::scratch::scratch;

    #[test]
    fn a_missing_text_is_created_random() {
        let dir = scratch("token-new");
        let (a, b) = (dir.path().join("a.txt"), dir.path().join("b.txt"));
        ensure_text(&a, "a").unwrap();
        ensure_text(&b, "b").unwrap();
        let token = fs::read_to_string(&a).unwrap();
        let token = token.trim_end();
        assert_eq!(token.len(), 32);
        assert!(token.bytes().all(|c| c.is_ascii_hexdigit()), "{token}");
        assert_ne!(token, fs::read_to_string(&b).unwrap().trim_end());
    }

    #[test]
    fn a_missing_key_is_32_random_bytes() {
        let dir = scratch("key-new");
        let path = dir.path().join("k.bin");
        ensure_bytes(&path, "k").unwrap();
        let key = fs::read(&path).unwrap();
        assert_eq!(key.len(), 32);
        assert_ne!(key, [0; 32]);
    }

    #[test]
    fn existing_secrets_are_kept() {
        let dir = scratch("secrets-kept");
        let (text, bytes) = (dir.path().join("t.txt"), dir.path().join("k.bin"));
        fs::write(&text, "mine\n").unwrap();
        fs::write(&bytes, [7; 32]).unwrap();
        ensure_text(&text, "t").unwrap();
        ensure_bytes(&bytes, "k").unwrap();
        assert_eq!(fs::read_to_string(&text).unwrap(), "mine\n");
        assert_eq!(fs::read(&bytes).unwrap(), [7; 32]);
    }
}
