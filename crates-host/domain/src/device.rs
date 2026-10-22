//! Сведения об устройстве — общие у узлов, которые отвечают на «кто ты»:
//! `HOST` (protobuf), `HTTP` (REST), `MQTT`.

/// Байт в заводском номере чипа STM32 (96 бит).
pub const UID_BYTES: usize = 12;

/// Сведения об устройстве.
///
/// Приходят снаружи: версию и коммит знает прошивка (`app`), чип — плата
/// (`bsp::ChipSpec`), а не `domain`. Пределы длины строк, если они есть, —
/// у того, кто их отдаёт: схема protobuf проверяет свои в
/// `host::Service::new`.
#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    /// Версия прошивки.
    pub version: &'static str,
    /// Короткий хеш коммита.
    pub commit: &'static str,
    /// Имя чипа.
    pub chip: &'static str,
    /// Заводской номер экземпляра чипа.
    pub uid: [u8; UID_BYTES],
    /// Объём flash чипа, байт.
    pub flash_size: u32,
    /// Частота ядра, Гц; `None` — неизвестна.
    pub sys_hz: Option<u32>,
}

/// Строка JSON в кавычках: `"` и `\` экранируются, управляющие символы —
/// `\u00XX`. Строки сведений задаёт сборка, но проект волен поставить свои —
/// с кавычкой внутри ответ без экранирования был бы не JSON.
///
/// # Errors
///
/// Ошибка записи в `out` (например, буфер короче строки).
pub fn write_json_string(out: &mut impl core::fmt::Write, s: &str) -> core::fmt::Result {
    out.write_char('"')?;
    for c in s.chars() {
        match c {
            '"' => out.write_str(r#"\""#)?,
            '\\' => out.write_str(r"\\")?,
            c if u32::from(c) < 0x20 => write!(out, r"\u{:04x}", u32::from(c))?,
            c => out.write_char(c)?,
        }
    }
    out.write_char('"')
}
// {%- if net_services != "" %}

/// Сведения об устройстве JSON-объектом: прошивка, чип, MAC, адрес и время
/// работы.
///
/// # Errors
///
/// Ошибка записи в `out`: объект не влез в буфер.
pub fn write_info(
    out: &mut impl core::fmt::Write,
    info: &DeviceInfo,
    mac: [u8; 6],
    ip: Option<embassy_net::Ipv4Address>,
    uptime_s: u64,
) -> core::fmt::Result {
    // Скобки объекта — `write_char`, а не удвоенные фигурные скобки в строке
    // формата: это исходник шаблона, и пару скобок Liquid принял бы за свой
    // тег.
    out.write_char('{')?;
    for (name, value) in [
        ("version", info.version),
        ("commit", info.commit),
        ("chip", info.chip),
    ] {
        write!(out, r#""{name}":"#)?;
        write_json_string(out, value)?;
        out.write_char(',')?;
    }
    out.write_str(r#""uid":""#)?;
    for b in info.uid {
        write!(out, "{b:02x}")?;
    }
    out.write_str(r#"","mac":""#)?;
    for (i, b) in mac.iter().enumerate() {
        write!(out, "{}{b:02x}", if i == 0 { "" } else { ":" })?;
    }
    out.write_str(r#"","ip":"#)?;
    match ip {
        Some(ip) => write!(out, r#""{ip}""#)?,
        None => out.write_str("null")?,
    }
    write!(out, r#","uptime_s":{uptime_s}"#)?;
    out.write_char('}')
}
// {%- endif %}

#[cfg(test)]
mod tests {
    use super::*;

    // {%- if net_services != "" %}
    #[test]
    fn info_is_a_json_object() {
        let info = DeviceInfo {
            version: "1.2.3",
            commit: "abc1234",
            chip: "STM32F407VE",
            uid: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 0xff],
            flash_size: 512 * 1024,
            sys_hz: Some(168_000_000),
        };
        let mut out = String::new();
        let mac = [2, 0, 0, 0, 0, 0x1f];
        write_info(
            &mut out,
            &info,
            mac,
            Some(embassy_net::Ipv4Address::new(10, 0, 0, 7)),
            42,
        )
        .unwrap();
        assert_eq!(
            out,
            r#"{"version":"1.2.3","commit":"abc1234","chip":"STM32F407VE","uid":"000102030405060708090aff","mac":"02:00:00:00:00:1f","ip":"10.0.0.7","uptime_s":42}"#
        );
        out.clear();
        write_info(&mut out, &info, mac, None, 0).unwrap();
        assert!(out.contains(r#""ip":null"#), "{out}");
    }
    // {%- endif %}

    /// Кавычки, обратная косая и управляющие символы в строках сведений не
    /// ломают JSON.
    #[test]
    fn strings_are_escaped() {
        let mut out = String::new();
        write_json_string(&mut out, "a\"b\\c\nd").unwrap();
        assert_eq!(out, r#""a\"b\\c\u000ad""#);
    }
}
