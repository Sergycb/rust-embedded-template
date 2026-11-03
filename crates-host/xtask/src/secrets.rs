//! Файлы сетевых служб и консоли, которые прошивка берёт из корня проекта:
//!
// {%- if net_services contains "rest" %}
//! - токен REST API;
// {%- endif %}
// {%- if has_console == "true" %}
//! - хэш пароля консоли (сам пароль — рядом, в git не идёт);
// {%- endif %}
// {%- if ssh_host_key == "file" %}
//! - вшиваемый ключ хоста SSH;
// {%- endif %}
// {%- if net_services contains "mqtt" %}
//! - адрес и вход брокера MQTT (`mqtt.toml`).
// {%- endif %}
//!
//! Создаются при первой сборке и коммитятся — почему, README, разделы
//! сетевых служб.
// {%- if has_console == "true" %}
//! Не коммитится только открытый пароль консоли: прошивке хватает его хэша.
// {%- endif %}
// {%- if net_services contains "rest" or has_console == "true" or ssh_host_key == "file" %}
//! Секреты — случайные; защита — уровня стенда.
// {%- endif %}
// {%- if net_services contains "ssh" %}
//! Для большего у SSH есть ключ хоста, созданный самой платой (`config`).
// {%- endif %}

// {%- if net_services contains "rest" or has_console == "true" %}
use std::fmt::Write as _;
// {%- endif %}
use std::{fs, path::Path};

use anyhow::Context;
// {%- if has_console == "true" %}
use sha2::{Digest, Sha256};
// {%- endif %}

// {%- if net_services contains "rest" %}
/// Токен `Authorization: Bearer` REST API (`domain::http`), 32 шестнадцатеричных
/// знака; его вшивает `crates-cross/app/src/net.rs`.
pub const REST_TOKEN_FILE: &str = "rest-token.txt";
// {%- endif %}

// {%- if has_console == "true" %}
/// Пароль консоли (`domain::console::PasswordHash`), 32 шестнадцатеричных
/// знака. Прошивке он не нужен — ей хватает
/// [`CONSOLE_PASSWORD_HASH_FILE`], — поэтому в `.gitignore`: его знает тот, кто
/// создал.
pub const CONSOLE_PASSWORD_FILE: &str = "console-password.txt";

/// Хэш пароля консоли, 48 байт как есть: 16 байт случайной соли и
/// SHA-256(соль ‖ пароль) — формат `domain::console::PasswordHash`.
/// Бинарный: `crates-cross/app/src/console.rs` вшивает его
/// `include_bytes!` без разбора. Коммитится: пароль из него не достать.
pub const CONSOLE_PASSWORD_HASH_FILE: &str = "console-password.hash";
// {%- endif %}

// {%- if ssh_host_key == "file" %}
/// Зерно ключа хоста SSH (ed25519, 32 байта), когда ключ вшит в прошивку, а не
/// создан платой; вшивает `crates-cross/app/src/net.rs`.
pub const SSH_HOST_KEY_FILE: &str = "ssh-host-key.bin";
// {%- endif %}

// {%- if net_services contains "mqtt" %}
/// Брокер MQTT (`domain::mqtt`): адрес, вход и префикс тем; разбирает и
/// вшивает `crates-cross/app/build.rs`.
pub const MQTT_CONFIG_FILE: &str = "mqtt.toml";

/// Заготовка `mqtt.toml`. Случайного в ней нет — адрес брокера вписывает
/// человек; `build.rs` её не отвергает, и прошивка с ней стучится по адресу
/// заготовки.
const MQTT_CONFIG_TEMPLATE: &str = r#"# Брокер MQTT, к которому подключается плата (`domain::mqtt`). Файл
# вшивается в прошивку при сборке (crates-cross/app/build.rs) и коммитится:
# пароль всё равно лежит открыто в каждом образе.

# Адрес брокера — IPv4: DNS у платы нет.
host = "192.168.1.10"
port = 1883
# Вход на брокер; пустой `user` — без имени и пароля. Вместе до 256 байт.
user = ""
password = ""
# Темы платы — `<prefix>/<uid>/…`: непустой, до 39 байт, без `/`, `+`, `#`,
# не с `$`.
prefix = "devices"
"#;
// {%- endif %}

/// Создаёт недостающие файлы служб. Существующие не трогает: в них уже чьи-то
/// значения, и прошивка с новыми отрезала бы тех, кто знает прежние.
pub fn ensure(root: &Path) -> Result<(), anyhow::Error> {
    // {%- if net_services contains "rest" %}
    ensure_text(&root.join(REST_TOKEN_FILE), "токен REST API")?;
    // {%- endif %}
    // {%- if has_console == "true" %}
    ensure_password_hash(
        &root.join(CONSOLE_PASSWORD_HASH_FILE),
        &root.join(CONSOLE_PASSWORD_FILE),
    )?;
    // {%- endif %}
    // {%- if ssh_host_key == "file" %}
    ensure_bytes(&root.join(SSH_HOST_KEY_FILE), "ключ хоста SSH")?;
    // {%- endif %}
    // {%- if net_services contains "mqtt" %}
    ensure_mqtt_config(&root.join(MQTT_CONFIG_FILE))?;
    // {%- endif %}
    Ok(())
}
// {%- if net_services contains "rest" %}

/// Случайный токен в текстовом файле.
fn ensure_text(path: &Path, what: &str) -> Result<(), anyhow::Error> {
    if path.exists() {
        return Ok(());
    }
    fs::write(path, format!("{}\n", random_hex()?))
        .with_context(|| format!("записать {}", path.display()))?;
    created(path, what);
    Ok(())
}
// {%- endif %}
// {%- if has_console == "true" %}

/// Хэш пароля консоли в `hash`. Есть — не трогает и пароля не ищет:
/// сборке хватает хэша. Нет — хэширует пароль из `password` (так меняют
/// пароль: свой — в файл, хэш — удалить), а нет и его — создаёт случайный и
/// печатает его один раз.
fn ensure_password_hash(hash: &Path, password: &Path) -> Result<(), anyhow::Error> {
    if hash.exists() {
        return Ok(());
    }
    let text = if password.exists() {
        let text = fs::read_to_string(password)
            .with_context(|| format!("прочитать {}", password.display()))?;
        let text = text.trim_ascii().to_owned();
        anyhow::ensure!(
            !text.is_empty(),
            "{} пуст: впишите пароль или удалите файл — `cargo xtask` создаст новый",
            password.display()
        );
        text
    } else {
        let text = random_hex()?;
        fs::write(password, format!("{text}\n"))
            .with_context(|| format!("записать {}", password.display()))?;
        println!(
            "создан {} — пароль консоли: {text}. В git он не идёт (.gitignore): \
             сохраните его, в прошивку вшит только хэш",
            password.display()
        );
        text
    };
    let salt = random::<16>()?;
    let digest = Sha256::new_with_prefix(salt).chain_update(text).finalize();
    fs::write(hash, [&salt[..], &digest[..]].concat())
        .with_context(|| format!("записать {}", hash.display()))?;
    created(hash, "хэш пароля консоли");
    Ok(())
}
// {%- endif %}
// {%- if net_services contains "rest" or has_console == "true" %}

/// 16 случайных байт шестнадцатеричной строкой — токен или пароль.
fn random_hex() -> Result<String, anyhow::Error> {
    let mut text = String::new();
    for b in random::<16>()? {
        write!(text, "{b:02x}").expect("запись в String не отказывает");
    }
    Ok(text)
}
// {%- endif %}
// {%- if ssh_host_key == "file" %}

/// 32 случайных байта как есть — зерно ключа.
fn ensure_bytes(path: &Path, what: &str) -> Result<(), anyhow::Error> {
    if path.exists() {
        return Ok(());
    }
    fs::write(path, random::<32>()?).with_context(|| format!("записать {}", path.display()))?;
    created(path, what);
    Ok(())
}
// {%- endif %}
// {%- if net_services contains "mqtt" %}

/// Заготовка `mqtt.toml`; существующий файл — уже чей-то, его не трогает.
fn ensure_mqtt_config(path: &Path) -> Result<(), anyhow::Error> {
    if path.exists() {
        return Ok(());
    }
    fs::write(path, MQTT_CONFIG_TEMPLATE)
        .with_context(|| format!("записать {}", path.display()))?;
    println!(
        "создан {} — впишите адрес брокера и закоммитьте файл",
        path.display()
    );
    Ok(())
}
// {%- endif %}
// {%- if net_services contains "rest" or has_console == "true" or ssh_host_key == "file" %}

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
// {%- endif %}

// {%- if net_services contains "mqtt" %}
/// Разбор `mqtt.toml` — тот же файл, что у build-скрипта прошивки: build-скрипт
/// `cargo test` не гоняет, поэтому разбор проверяется здесь.
#[cfg(test)]
#[path = "../../../crates-cross/app/build/mqtt.rs"]
mod mqtt_toml;
// {%- endif %}

#[cfg(test)]
mod tests {
    use std::fs;

    // {%- if net_services contains "rest" %}
    use super::ensure_text;
    // {%- endif %}
    // {%- if has_console == "true" %}
    use sha2::{Digest, Sha256};

    use super::ensure_password_hash;
    // {%- endif %}
    // {%- if ssh_host_key == "file" %}
    use super::ensure_bytes;
    // {%- endif %}
    // {%- if net_services contains "mqtt" %}
    use super::{MQTT_CONFIG_TEMPLATE, ensure_mqtt_config};
    // {%- endif %}
    use crate::memory::scratch::scratch;
    // {%- if net_services contains "rest" %}

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
    // {%- endif %}
    // {%- if has_console == "true" %}

    /// Соль и SHA-256(соль ‖ `password`) — то, что сверяет
    /// `domain::console::PasswordHash`.
    fn assert_hash_of(hash: &[u8], password: &str) {
        assert_eq!(hash.len(), 48);
        let (salt, digest) = hash.split_at(16);
        let expected = Sha256::new_with_prefix(salt)
            .chain_update(password)
            .finalize();
        assert_eq!(digest, &expected[..], "хэш не от {password:?}");
    }

    /// Нет ни хэша, ни пароля — случайный пароль в текстовом файле и его хэш
    /// со случайной солью.
    #[test]
    fn a_missing_password_is_created_with_its_hash() {
        let dir = scratch("console-password-new");
        let (hash, password) = (dir.path().join("p.hash"), dir.path().join("p.txt"));
        ensure_password_hash(&hash, &password).unwrap();
        let text = fs::read_to_string(&password).unwrap();
        let text = text.trim_end();
        assert_eq!(text.len(), 32);
        assert!(text.bytes().all(|c| c.is_ascii_hexdigit()), "{text}");
        let first = fs::read(&hash).unwrap();
        assert_hash_of(&first, text);

        // Тот же пароль заново — другая соль.
        fs::remove_file(&hash).unwrap();
        ensure_password_hash(&hash, &password).unwrap();
        let second = fs::read(&hash).unwrap();
        assert_hash_of(&second, text);
        assert_ne!(first[..16], second[..16]);
    }

    /// Хэша нет, а пароль есть — хэшируется он, а не новый; файл пароля не
    /// трогается. Пустой — ошибка, а не хэш пустого пароля.
    #[test]
    fn an_existing_password_is_hashed() {
        let dir = scratch("console-password-own");
        let (hash, password) = (dir.path().join("p.hash"), dir.path().join("p.txt"));
        fs::write(&password, "mine\n").unwrap();
        ensure_password_hash(&hash, &password).unwrap();
        assert_hash_of(&fs::read(&hash).unwrap(), "mine");
        assert_eq!(fs::read_to_string(&password).unwrap(), "mine\n");

        let (hash, password) = (dir.path().join("e.hash"), dir.path().join("e.txt"));
        fs::write(&password, " \n").unwrap();
        let err = ensure_password_hash(&hash, &password).unwrap_err();
        assert!(err.to_string().contains("пуст"), "{err}");
        assert!(!hash.exists());
    }
    // {%- endif %}
    // {%- if ssh_host_key == "file" %}

    #[test]
    fn a_missing_key_is_32_random_bytes() {
        let dir = scratch("key-new");
        let path = dir.path().join("k.bin");
        ensure_bytes(&path, "k").unwrap();
        let key = fs::read(&path).unwrap();
        assert_eq!(key.len(), 32);
        assert_ne!(key, [0; 32]);
    }
    // {%- endif %}

    #[test]
    fn existing_files_are_kept() {
        let dir = scratch("secrets-kept");
        // {%- if net_services contains "rest" %}
        let text = dir.path().join("t.txt");
        fs::write(&text, "mine\n").unwrap();
        ensure_text(&text, "t").unwrap();
        assert_eq!(fs::read_to_string(&text).unwrap(), "mine\n");
        // {%- endif %}
        // {%- if has_console == "true" %}
        // Хэш есть, а пароля нет — сборка идёт по хэшу, нового пароля нет.
        let (hash, password) = (dir.path().join("p.hash"), dir.path().join("p.txt"));
        fs::write(&hash, [7; 48]).unwrap();
        ensure_password_hash(&hash, &password).unwrap();
        assert_eq!(fs::read(&hash).unwrap(), [7; 48]);
        assert!(!password.exists());
        // {%- endif %}
        // {%- if ssh_host_key == "file" %}
        let bytes = dir.path().join("k.bin");
        fs::write(&bytes, [7; 32]).unwrap();
        ensure_bytes(&bytes, "k").unwrap();
        assert_eq!(fs::read(&bytes).unwrap(), [7; 32]);
        // {%- endif %}
        // {%- if net_services contains "mqtt" %}
        let mqtt = dir.path().join("mqtt.toml");
        fs::write(&mqtt, "host = \"10.0.0.1\"\n").unwrap();
        ensure_mqtt_config(&mqtt).unwrap();
        assert_eq!(fs::read_to_string(&mqtt).unwrap(), "host = \"10.0.0.1\"\n");
        // {%- endif %}
    }
    // {%- if net_services contains "mqtt" %}

    #[test]
    fn a_missing_mqtt_config_is_a_template() {
        let dir = scratch("mqtt-new");
        let path = dir.path().join("mqtt.toml");
        ensure_mqtt_config(&path).unwrap();
        let text = fs::read_to_string(&path).unwrap();
        assert_eq!(text, MQTT_CONFIG_TEMPLATE);
        assert!(text.contains("host = \"192.168.1.10\""), "{text}");
        assert!(text.contains("prefix = \"devices\""), "{text}");
    }

    /// Заготовка проходит разбор build-скрипта как есть.
    #[test]
    fn the_mqtt_template_parses() {
        let config = super::mqtt_toml::parse_mqtt_toml(MQTT_CONFIG_TEMPLATE).unwrap();
        assert_eq!(config.host.octets(), [192, 168, 1, 10]);
        assert_eq!(config.port, 1883);
        assert_eq!(config.user, "");
        assert_eq!(config.password, "");
        assert_eq!(config.prefix, "devices");
    }

    /// Битый адрес, порт 0, недопустимый префикс, пароль без имени, слишком
    /// длинный вход, лишний или пропущенный ключ — ошибка, и в её тексте имя
    /// поля.
    #[test]
    fn bad_mqtt_toml_is_rejected() {
        use std::fmt::Write as _;

        use super::mqtt_toml::parse_mqtt_toml;

        let good = [
            ("host", "\"10.0.0.1\""),
            ("port", "1883"),
            ("user", "\"\""),
            ("password", "\"\""),
            ("prefix", "\"devices\""),
        ];
        // Файл из `good`, где у `key` значение `value` (`None` — ключа нет).
        let with = |key: &str, value: Option<&str>| {
            let mut text = String::new();
            for (k, v) in good {
                if let Some(v) = if k == key { value } else { Some(v) } {
                    writeln!(text, "{k} = {v} # комментарий")
                        .expect("запись в String не отказывает");
                }
            }
            text
        };
        let long = format!("\"{}\"", "p".repeat(40));
        // Имя и пароль вместе длиннее `AUTH_MAX` (256).
        let long_user = format!("\"{}\"", "u".repeat(257));
        for (key, value) in [
            ("host", Some("\"300.1.1.1\"")),
            ("host", Some("\"broker.local\"")),
            ("host", Some("10.0.0.1")),
            ("host", None),
            ("port", Some("0")),
            ("port", Some("65536")),
            ("port", None),
            ("prefix", Some("\"\"")),
            ("prefix", Some("\"a/b\"")),
            ("prefix", Some("\"a+\"")),
            ("prefix", Some("\"#\"")),
            ("prefix", Some(long.as_str())),
            ("prefix", Some("\"$SYS\"")),
            ("password", Some("\"secret\"")),
            ("user", Some(long_user.as_str())),
        ] {
            let err = parse_mqtt_toml(&with(key, value)).unwrap_err();
            assert!(err.contains(key), "{key} = {value:?}: {err}");
        }
        let unknown = format!("{}broker = \"x\"\n", with("", None));
        let err = parse_mqtt_toml(&unknown).unwrap_err();
        assert!(err.contains("broker"), "{err}");

        let longest = format!("\"{}\"", "p".repeat(39));
        let config = parse_mqtt_toml(&with("prefix", Some(&longest))).unwrap();
        assert_eq!(config.prefix.len(), 39);
        // `#` в кавычках — часть значения, а не комментарий.
        let hash = parse_mqtt_toml(&with("user", Some("\"a#b\""))).unwrap();
        assert_eq!(hash.user, "a#b");
        // Ровно `AUTH_MAX` — можно.
        let auth = format!("\"{}\"", "u".repeat(256));
        let auth = parse_mqtt_toml(&with("user", Some(&auth))).unwrap();
        assert_eq!(auth.user.len(), 256);
    }
    // {%- endif %}
}
