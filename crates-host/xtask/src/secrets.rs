//! Секреты сетевых служб, которые прошивка берёт из корня проекта: токен
//! REST API, пароль консоли SSH и вшиваемый ключ хоста SSH, — и настройки
//! брокера MQTT (`mqtt.toml`).
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
    if has_net_service("mqtt") {
        ensure_mqtt_config(&root.join(MQTT_CONFIG_FILE))?;
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

/// Заготовка настроек брокера MQTT; существующий файл — уже чьи-то настройки,
/// его не трогает.
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

    use super::{MQTT_CONFIG_TEMPLATE, ensure_bytes, ensure_mqtt_config, ensure_text};
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
        let mqtt = dir.path().join("mqtt.toml");
        fs::write(&text, "mine\n").unwrap();
        fs::write(&bytes, [7; 32]).unwrap();
        fs::write(&mqtt, "host = \"10.0.0.1\"\n").unwrap();
        ensure_text(&text, "t").unwrap();
        ensure_bytes(&bytes, "k").unwrap();
        ensure_mqtt_config(&mqtt).unwrap();
        assert_eq!(fs::read_to_string(&text).unwrap(), "mine\n");
        assert_eq!(fs::read(&bytes).unwrap(), [7; 32]);
        assert_eq!(fs::read_to_string(&mqtt).unwrap(), "host = \"10.0.0.1\"\n");
    }

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
    // {%- if net_services contains "mqtt" %}

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
