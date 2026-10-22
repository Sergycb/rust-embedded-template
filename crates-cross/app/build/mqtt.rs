//! Разбор `mqtt.toml` — настроек брокера MQTT, которые `build.rs` вшивает в
//! прошивку (`crate::net::MQTT_BROKER`, `MQTT_PREFIX`).
//!
//! Без Liquid: файл подключает и тест `xtask` (`crates-host/xtask/src/secrets.rs`),
//! а корневой workspace компилируется и в репозитории шаблона. Разбор — свой, на
//! пять ключей: build-зависимость ради TOML была бы дороже его самого.

use std::net::Ipv4Addr;

/// Самый длинный префикс тем — `domain::mqtt::PREFIX_MAX`: build-скрипт `domain`
/// не видит, равенство держит `const`-assert в `crate::net`.
const PREFIX_MAX: usize = 39;

/// Настройки брокера.
#[derive(Debug)]
pub struct MqttConfig {
    /// Адрес брокера.
    pub host: Ipv4Addr,
    /// Порт брокера, не 0.
    pub port: u16,
    /// Имя на брокере; пустое — вход без имени и пароля.
    pub user: String,
    /// Пароль на брокере.
    pub password: String,
    /// Префикс тем: непустой, не длиннее [`PREFIX_MAX`], без `/`, `+`, `#`.
    pub prefix: String,
}

/// Разбирает `mqtt.toml`: строки `ключ = значение`, `#` вне кавычек — комментарий
/// до конца строки, строки — в `"…"` без экранирования, `port` — число. Ошибка
/// называет поле.
pub fn parse_mqtt_toml(text: &str) -> Result<MqttConfig, String> {
    const KEYS: [&str; 5] = ["host", "port", "user", "password", "prefix"];
    let mut values: [Option<&str>; 5] = [None; 5];
    for (n, line) in text.lines().enumerate() {
        let line = strip_comment(line).trim();
        if line.is_empty() {
            continue;
        }
        let (key, value) = line
            .split_once('=')
            .ok_or_else(|| format!("строка {}: ожидается `ключ = значение`", n + 1))?;
        let key = key.trim();
        let slot = KEYS
            .iter()
            .position(|&k| k == key)
            .ok_or_else(|| format!("строка {}: неизвестный ключ `{key}`", n + 1))?;
        if values[slot].replace(value.trim()).is_some() {
            return Err(format!("{key}: задан дважды"));
        }
    }
    let [host, port, user, password, prefix] = values;

    let host = string("host", host)?;
    let host = host
        .parse::<Ipv4Addr>()
        .map_err(|err| format!("host: «{host}» — не адрес IPv4 ({err}; DNS у платы нет)"))?;
    let port = port.ok_or("нет ключа `port`")?;
    let port = port
        .parse::<u16>()
        .ok()
        .filter(|&port| port != 0)
        .ok_or_else(|| format!("port: «{port}» — ожидается число от 1 до 65535"))?;
    let user = string("user", user)?.to_string();
    let password = string("password", password)?.to_string();
    let prefix = string("prefix", prefix)?;
    if prefix.is_empty() || prefix.len() > PREFIX_MAX {
        return Err(format!(
            "prefix: «{prefix}» — нужен непустой префикс не длиннее {PREFIX_MAX} байт"
        ));
    }
    if prefix.contains(['/', '+', '#']) {
        return Err(format!(
            "prefix: «{prefix}» — `/`, `+` и `#` в префиксе темы недопустимы"
        ));
    }
    Ok(MqttConfig {
        host,
        port,
        user,
        password,
        prefix: prefix.to_string(),
    })
}

/// Строка без комментария: `#` вне кавычек и всё за ним.
fn strip_comment(line: &str) -> &str {
    let mut quoted = false;
    for (i, c) in line.char_indices() {
        match c {
            '"' => quoted = !quoted,
            '#' if !quoted => return &line[..i],
            _ => {}
        }
    }
    line
}

/// Значение строкового ключа: есть, в кавычках, без кавычек и `\` внутри.
fn string<'a>(key: &str, value: Option<&'a str>) -> Result<&'a str, String> {
    let value = value.ok_or_else(|| format!("нет ключа `{key}`"))?;
    value
        .strip_prefix('"')
        .and_then(|v| v.strip_suffix('"'))
        .filter(|v| !v.contains(['"', '\\']))
        .ok_or_else(|| {
            format!("{key}: ожидается строка в кавычках без `\"` и `\\` внутри, а не {value}")
        })
}
