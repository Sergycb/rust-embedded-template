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
// {%- if has_cli == "true" %}
//! `CONSOLE` (консоль `cli`).
// {%- endif %}

/// Байт в заводском номере чипа STM32 (96 бит).
pub const UID_BYTES: usize = 12;

/// Предел [`panic_brief`] для [`DeviceInfo::last_panic`], байт: место и
/// первая строка сообщения обычно короче, а дольше строку не держит ни
/// одна схема ответа.
pub const PANIC_BRIEF: usize = 96;
/// Самый длинный вывод [`write_info`], байт: 256 — остальные поля с запасом
/// под длинную версию, 27 — `reset independent_watchdog`, остальное — строка
/// `last panic` с [`PANIC_BRIEF`] байт. Под него заводятся буферы `info` в
/// консоли, REST и MQTT.
pub const INFO_TEXT: usize = 256 + 27 + "last panic \n".len() + PANIC_BRIEF;

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
    /// Причина последнего сброса в `snake_case` (`pin`, `software`,
    /// `independent_watchdog`…) — как её называет `bsp::reset::take_cause`.
    pub reset: &'static str,
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
/// символе, а не только на `\n`: управляющий символ ломает вывод `info`.
/// Так же и место: управляющий символ в нём (сообщение не в формате
/// паникёра) кончает строку, текст тогда не берётся.
#[must_use]
pub fn panic_brief(m: &str, max: usize) -> &str {
    let m = m.strip_prefix("panicked at ").unwrap_or(m);
    let (location, text) = m.split_once('\n').unwrap_or((m, ""));
    let clean = location.find(char::is_control).unwrap_or(location.len());
    let start = location[..clean].rfind(['/', '\\']).map_or(0, |i| i + 1);
    let end = if clean < location.len() || m.len() == location.len() {
        clean
    } else {
        location.len() + 1 + text.find(char::is_control).unwrap_or(text.len())
    };
    let brief = &m[start..end];
    &brief[..brief.floor_char_boundary(max)]
}
/// Сведения об устройстве текстом: прошивка, чип, MAC, адрес, время работы,
/// причина сброса и прошлая паника.
///
/// # Errors
///
/// Ошибка записи в `out`: сведения не влезли в буфер.
pub fn write_info(
    out: &mut impl core::fmt::Write,
    info: &DeviceInfo,
    mac: Option<[u8; 6]>,
    ip: Option<core::net::Ipv4Addr>,
    uptime_s: u64,
) -> core::fmt::Result {
    writeln!(out, "version {} ({})", info.version, info.commit)?;
    write!(
        out,
        "chip {}, flash {} KiB",
        info.chip,
        info.flash_size / 1024
    )?;
    match info.sys_hz {
        Some(hz) => writeln!(out, ", core {} MHz", hz / 1_000_000)?,
        None => out.write_char('\n')?,
    }
    out.write_str("uid ")?;
    for b in info.uid {
        write!(out, "{b:02x}")?;
    }
    out.write_str("\nmac ")?;
    match mac {
        Some(mac) => {
            for (i, b) in mac.iter().enumerate() {
                write!(out, "{}{b:02x}", if i == 0 { "" } else { ":" })?;
            }
            out.write_char('\n')?;
        }
        None => out.write_str("none\n")?,
    }
    match ip {
        Some(ip) => writeln!(out, "ip {ip}")?,
        None => out.write_str("ip none\n")?,
    }
    writeln!(out, "uptime {uptime_s} s")?;
    writeln!(out, "reset {}", info.reset)?;
    // Место и текст паники разделяет её же перевод строки, а консоль держит
    // «ключ значение» по строке: текст идёт через пробел, а не второй строкой
    // без ключа.
    let (place, text) = match info.last_panic {
        Some(panic) => panic.split_once('\n').unwrap_or((panic, "")),
        None => ("none", ""),
    };
    out.write_str("last panic ")?;
    out.write_str(place)?;
    if !text.is_empty() {
        write!(out, " {text}")?;
    }
    out.write_char('\n')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn info_carries_every_field() {
        let info = DeviceInfo {
            version: "1.2.3",
            commit: "abc1234",
            chip: "STM32F407VE",
            uid: [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 0xff],
            flash_size: 512 * 1024,
            sys_hz: Some(168_000_000),
            reset: "pin",
            last_panic: None,
        };
        let mut out = String::new();
        let mac = [2, 0, 0, 0, 0, 0x1f];
        write_info(
            &mut out,
            &info,
            Some(mac),
            Some(core::net::Ipv4Addr::new(10, 0, 0, 7)),
            42,
        )
        .unwrap();
        assert!(out.starts_with("version 1.2.3 (abc1234)\n"), "{out}");
        assert!(
            out.contains("chip STM32F407VE, flash 512 KiB, core 168 MHz\n"),
            "{out}"
        );
        assert!(out.contains("uid 000102030405060708090aff\n"), "{out}");
        assert!(out.contains("mac 02:00:00:00:00:1f\n"), "{out}");
        assert!(out.contains("ip 10.0.0.7\n"), "{out}");
        assert!(out.contains("uptime 42 s\n"), "{out}");
        assert!(out.contains("reset pin\n"), "{out}");
        assert!(out.ends_with("last panic none\n"), "{out}");
        out.clear();
        write_info(&mut out, &info, None, None, 0).unwrap();
        assert!(out.contains("mac none\n"), "{out}");
        assert!(out.contains("ip none\n"), "{out}");
    }

    /// Самые длинные сведения, какие отдаёт плата, помещаются в [`INFO_TEXT`].
    #[test]
    fn the_longest_info_fits_the_buffer() {
        let panic = format!(
            "panicked at {}\n{}",
            "a".repeat(PANIC_BRIEF / 2),
            "b".repeat(PANIC_BRIEF)
        );
        let brief = panic_brief(&panic, PANIC_BRIEF);
        assert_eq!(brief.len(), PANIC_BRIEF);
        let info = DeviceInfo {
            version: "65535.65535.65535-rc.65535",
            commit: "0123456789ab",
            chip: "STM32H7R7L8HXH",
            uid: [0xff; UID_BYTES],
            flash_size: u32::MAX,
            sys_hz: None,
            reset: "independent_watchdog",
            last_panic: Some(String::leak(brief.to_owned())),
        };
        let mut out = String::new();
        write_info(
            &mut out,
            &info,
            Some([0xff; 6]),
            Some(core::net::Ipv4Addr::BROADCAST),
            u64::MAX,
        )
        .unwrap();
        assert!(out.len() <= INFO_TEXT, "{} > {INFO_TEXT}", out.len());
    }

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
        // И в месте, если перевода строки нет: каталог до символа отрезан,
        // за ним — ничего.
        assert_eq!(
            panic_brief("a/x.rs:1:1: ab\tcd", PANIC_BRIEF),
            "x.rs:1:1: ab"
        );
        assert_eq!(panic_brief("a\tb/x.rs\n", PANIC_BRIEF), "a");
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
}
