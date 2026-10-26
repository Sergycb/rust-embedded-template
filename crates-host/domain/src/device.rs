//! Сведения об устройстве — общие у узлов, которые отвечают на «кто ты».
// {%- if has_protobuf == "true" %}
//! `PROTOBUF` (protobuf).
// {%- endif %}
// {%- if net_services contains "rest" %}
//! `HTTP` (REST).
// {%- endif %}
// {%- if net_services contains "ssh" %}
//! `SSH`.
// {%- endif %}
// {%- if net_services contains "mqtt" %}
//! `MQTT`.
// {%- endif %}

pub use ports::ResetCause;

/// Байт в заводском номере чипа STM32 (96 бит).
pub const UID_BYTES: usize = 12;

/// Предел [`panic_brief`] для [`DeviceInfo::last_panic`], байт: место и
/// первая строка сообщения обычно короче, а дольше строку не держит ни
/// одна схема ответа.
pub const PANIC_BRIEF: usize = 96;
// {%- if net_services != "" %}

/// Самый длинный объект [`write_info`], байт: 256 — прошлые поля с запасом
/// под длинную версию, 243 — `reset` (31) и худший `last_panic` (14 и 198:
/// [`PANIC_BRIEF`] байт из кавычек по два и перевода строки в шесть).
// {%- if net_services contains "rest" %}
/// Под него — буфер ответа REST.
// {%- endif %}
// {%- if net_services contains "mqtt" %}
/// Под него — тело `info` MQTT.
// {%- endif %}
pub const INFO_MAX: usize = 499;
// {%- endif %}

/// Сведения об устройстве.
///
/// Приходят снаружи: версию и коммит знает прошивка (`app`), чип — плата
/// (`bsp::ChipSpec`), а не `domain`. Пределы длины строк, если они есть, —
/// у того, кто их отдаёт.
// {%- if has_protobuf == "true" %}
/// Схема protobuf проверяет свои в `protobuf::Service::new`.
// {%- endif %}
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
    /// Причина последнего сброса.
    pub reset: ResetCause,
    /// Прошлая паника, сжатая [`panic_brief`] до [`PANIC_BRIEF`] байт;
    /// `None` — её не было или сборка отладочная (там дамп никто не пишет).
    pub last_panic: Option<&'static str>,
}

/// Сообщение паники `panic-persist` коротко: место без каталогов и первая
/// строка текста, не длиннее `max` байт (по границе символа).
///
/// Из `panicked at src/net/dhcp.rs:12:5:\nmessage\n…` остаётся
/// `dhcp.rs:12:5:\nmessage` — срез исходной строки, поэтому место и текст
/// разделяет её же перевод строки. Текст обрезается на первом управляющем
/// символе, а не только на `\n`: в JSON каждый такой символ стоит шести байт.
#[must_use]
pub fn panic_brief(m: &str, max: usize) -> &str {
    let m = m.strip_prefix("panicked at ").unwrap_or(m);
    let (location, text) = m.split_once('\n').unwrap_or((m, ""));
    let start = location.rfind(['/', '\\']).map_or(0, |i| i + 1);
    let end = if m.len() == location.len() {
        m.len()
    } else {
        location.len() + 1 + text.find(char::is_control).unwrap_or(text.len())
    };
    let brief = &m[start..end];
    &brief[..brief.floor_char_boundary(max)]
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

/// Сведения об устройстве JSON-объектом: прошивка, чип, MAC, адрес, время
/// работы, причина сброса и прошлая паника.
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
    write!(
        out,
        r#","uptime_s":{uptime_s},"reset":"{}","#,
        info.reset.as_str()
    )?;
    out.write_str(r#""last_panic":"#)?;
    match info.last_panic {
        Some(m) => write_json_string(out, m)?,
        None => out.write_str("null")?,
    }
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
            reset: ResetCause::Pin,
            last_panic: None,
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
            r#"{"version":"1.2.3","commit":"abc1234","chip":"STM32F407VE","uid":"000102030405060708090aff","mac":"02:00:00:00:00:1f","ip":"10.0.0.7","uptime_s":42,"reset":"pin","last_panic":null}"#
        );
        out.clear();
        write_info(&mut out, &info, mac, None, 0).unwrap();
        assert!(out.contains(r#""ip":null"#), "{out}");
        out.clear();
        let info = DeviceInfo {
            reset: ResetCause::Software,
            last_panic: Some("main.rs:7:1:\nboom \"x\""),
            ..info
        };
        write_info(&mut out, &info, mac, None, 0).unwrap();
        assert!(
            out.ends_with(r#""reset":"software","last_panic":"main.rs:7:1:\u000aboom \"x\""}"#),
            "{out}"
        );
    }

    /// Самые длинные сведения, какие отдаёт плата, помещаются в [`INFO_MAX`]:
    /// худшая паника — место и текст из кавычек (каждая — два байта JSON),
    /// разделённые переводом строки (шесть).
    #[test]
    fn the_longest_info_fits_the_buffer() {
        let quotes = "\"".repeat(PANIC_BRIEF);
        let panic = format!("panicked at {}\n{quotes}", &quotes[..PANIC_BRIEF / 2]);
        let brief = panic_brief(&panic, PANIC_BRIEF);
        assert_eq!((brief.len(), brief.matches('\n').count()), (PANIC_BRIEF, 1));
        let info = DeviceInfo {
            version: "65535.65535.65535-rc.65535",
            commit: "0123456789ab",
            chip: "STM32H7R7L8HXH",
            uid: [0xff; UID_BYTES],
            flash_size: u32::MAX,
            sys_hz: None,
            reset: ResetCause::IndependentWatchdog,
            last_panic: Some(String::leak(brief.to_owned())),
        };
        let mut out = String::new();
        write_info(
            &mut out,
            &info,
            [0xff; 6],
            Some(embassy_net::Ipv4Address::BROADCAST),
            u64::MAX,
        )
        .unwrap();
        assert!(out.len() <= INFO_MAX, "{} > {INFO_MAX}", out.len());
    }
    // {%- endif %}

    #[test]
    fn panic_brief_keeps_the_file_and_the_first_line() {
        let m = "panicked at src/net/dhcp.rs:12:5:\nlease lost\nsecond line";
        assert_eq!(panic_brief(m, PANIC_BRIEF), "dhcp.rs:12:5:\nlease lost");
        let m = "panicked at C:\\proj\\src\\main.rs:1:2:\nboom";
        assert_eq!(panic_brief(m, PANIC_BRIEF), "main.rs:1:2:\nboom");
        // Без префикса и без перевода строки — только место.
        assert_eq!(panic_brief("a/b/c.rs:3:4:", PANIC_BRIEF), "c.rs:3:4:");
        // Управляющий символ в тексте обрывает его так же, как `\n`.
        assert_eq!(
            panic_brief("x.rs:1:1:\nab\tcd", PANIC_BRIEF),
            "x.rs:1:1:\nab"
        );
        assert_eq!(panic_brief("", PANIC_BRIEF), "");
        assert_eq!(panic_brief("panicked at ", PANIC_BRIEF), "");
    }

    /// Обрезка не рвёт многобайтный символ: «ж» — два байта, и предел посреди
    /// него оставляет строку на байт короче.
    #[test]
    fn panic_brief_cuts_on_a_char_boundary() {
        let m = "m.rs:1:1:\nжжж";
        assert_eq!(panic_brief(m, 13), "m.rs:1:1:\nж");
        assert_eq!(panic_brief(m, 12), "m.rs:1:1:\nж");
        assert_eq!(panic_brief(m, 11), "m.rs:1:1:\n");
        assert_eq!(panic_brief(m, 0), "");
    }

    /// Кавычки, обратная косая и управляющие символы в строках сведений не
    /// ломают JSON.
    #[test]
    fn strings_are_escaped() {
        let mut out = String::new();
        write_json_string(&mut out, "a\"b\\c\nd").unwrap();
        assert_eq!(out, r#""a\"b\\c\u000ad""#);
    }
}
