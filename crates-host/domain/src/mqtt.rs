//! Узел `MQTT` графа задач: клиент брокера (MQTT 5, `minimq`) поверх сокета
//! стека `embassy-net`.
//!
//! Плата сама подключается к брокеру и держит соединение; все темы лежат под
//! `<prefix>/<uid>/`, где `uid` — заводской номер чипа в hex, а `prefix`
//! задаёт сборка.
//!
//! | Тема | Направление | Содержимое |
//! |---|---|---|
//! | `status` | плата → брокер | `online`/`offline` (retain; `offline` — will) |
//! | `info` | плата → брокер | сведения об устройстве, JSON (retain) |
//! | `telemetry` | плата → брокер | время работы раз в [`TELEMETRY_INTERVAL`](crate::mqtt::TELEMETRY_INTERVAL), JSON |
//! | `cmd/reboot` | брокер → плата | перезагрузка |
//! | `cmd/reboot/result` | плата → брокер | исход команды |
//!
//! Здесь — чистая часть без ввода-вывода: имена тем, разбор команд, тексты
//! сообщений и адрес брокера.

use embassy_time::Duration;
use heapless::String;

use crate::device::UID_BYTES;

/// Самый длинный префикс тем: вместе с `-` и номером чипа (24 hex) он даёт
/// 64 байта — предел идентификатора клиента.
pub const PREFIX_MAX: usize = 39;

/// Как часто плата публикует `telemetry`.
pub const TELEMETRY_INTERVAL: Duration = Duration::from_secs(10);

/// Hex-знаков в номере чипа.
const UID_HEX: usize = 2 * UID_BYTES;
/// Идентификатор клиента: `<prefix>-<uid>`.
const CLIENT_ID_LEN: usize = PREFIX_MAX + 1 + UID_HEX;
/// `<prefix>/<uid>` и самая длинная тема под ним (`/cmd/reboot/result`).
const TOPIC_LEN: usize = CLIENT_ID_LEN + "/cmd/reboot/result".len();

const REBOOT: &str = "/cmd/reboot";

/// Куда подключаться.
#[derive(Debug, Clone, Copy)]
pub struct Endpoint {
    /// IPv4-адрес брокера.
    pub addr: [u8; 4],
    /// Порт брокера.
    pub port: u16,
    /// Имя пользователя; пустое — без входа.
    pub user: &'static str,
    /// Пароль.
    pub password: &'static str,
}

/// Откуда узел берёт адрес брокера перед каждым подключением.
#[expect(
    async_fn_in_trait,
    reason = "узел MQTT зовёт адрес на одном исполнителе: `Send` у future не нужен"
)]
pub trait Broker {
    /// Адрес брокера на этот раз.
    async fn endpoint(&mut self) -> Endpoint;
}

/// Адрес, вшитый в прошивку.
#[derive(Debug, Clone, Copy)]
pub struct Fixed(pub Endpoint);

impl Broker for Fixed {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "сигнатуру задаёт трейт `Broker`; вшитому адресу ждать нечего"
    )]
    async fn endpoint(&mut self) -> Endpoint {
        self.0
    }
}

/// Имена тем и идентификатор клиента одной платы.
#[derive(Debug, Clone)]
pub struct Topics {
    prefix: String<PREFIX_MAX>,
    uid: String<UID_HEX>,
}

impl Topics {
    /// Темы под `<prefix>/<uid>`.
    ///
    /// # Panics
    ///
    /// `prefix` длиннее [`PREFIX_MAX`]: сборка это уже проверила.
    #[must_use]
    pub fn new(prefix: &str, uid: &[u8; UID_BYTES]) -> Self {
        use core::fmt::Write;
        let mut hex = String::new();
        for b in uid {
            write!(hex, "{b:02x}").expect("номер чипа в hex влезает");
        }
        Self {
            prefix: String::try_from(prefix).expect("префикс не длиннее PREFIX_MAX"),
            uid: hex,
        }
    }

    fn topic<const N: usize>(&self, suffix: &str) -> String<N> {
        use core::fmt::Write;
        let mut s = String::new();
        write!(s, "{}/{}{suffix}", self.prefix, self.uid).expect("тема влезает в N");
        s
    }

    /// Тема состояния.
    #[must_use]
    pub fn status(&self) -> String<{ CLIENT_ID_LEN + "/status".len() }> {
        self.topic("/status")
    }

    /// Тема сведений об устройстве.
    #[must_use]
    pub fn info(&self) -> String<{ CLIENT_ID_LEN + "/info".len() }> {
        self.topic("/info")
    }

    /// Тема телеметрии.
    #[must_use]
    pub fn telemetry(&self) -> String<{ CLIENT_ID_LEN + "/telemetry".len() }> {
        self.topic("/telemetry")
    }

    /// Фильтр подписки на все команды.
    #[must_use]
    pub fn cmd_filter(&self) -> String<{ CLIENT_ID_LEN + "/cmd/#".len() }> {
        self.topic("/cmd/#")
    }

    /// Тема исхода `reboot`.
    #[must_use]
    pub fn reboot_result(&self) -> String<TOPIC_LEN> {
        self.topic("/cmd/reboot/result")
    }

    /// Идентификатор клиента: `<prefix>-<uid>`.
    #[must_use]
    pub fn client_id(&self) -> String<CLIENT_ID_LEN> {
        use core::fmt::Write;
        let mut s = String::new();
        write!(s, "{}-{}", self.prefix, self.uid).expect("идентификатор влезает");
        s
    }
}

/// Команда с брокера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Перезагрузить МК.
    Reboot,
}

/// Команда по теме сообщения; `None` — чужая или неизвестная тема.
#[must_use]
pub fn parse_command(topics: &Topics, topic: &str) -> Option<Command> {
    let rest = topic
        .strip_prefix(topics.prefix.as_str())?
        .strip_prefix('/')?
        .strip_prefix(topics.uid.as_str())?;
    match rest {
        REBOOT => Some(Command::Reboot),
        _ => None,
    }
}

/// Тело `telemetry`: JSON-объект со временем работы.
///
/// # Errors
///
/// Ошибка записи в `out`: объект не влез в буфер.
pub fn write_telemetry(out: &mut impl core::fmt::Write, uptime_s: u64) -> core::fmt::Result {
    // Скобки — `write_char`: это исходник шаблона, см. `device::write_info`.
    out.write_char('{')?;
    write!(out, r#""uptime_s":{uptime_s}"#)?;
    out.write_char('}')
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn topics_are_prefix_uid_suffix() {
        let t = Topics::new("devices", &[0xab; 12]);
        let uid = "abababababababababababab";
        assert_eq!(t.status().as_str(), format!("devices/{uid}/status"));
        assert_eq!(t.info().as_str(), format!("devices/{uid}/info"));
        assert_eq!(t.telemetry().as_str(), format!("devices/{uid}/telemetry"));
        assert_eq!(t.cmd_filter().as_str(), format!("devices/{uid}/cmd/#"));
        assert_eq!(
            t.reboot_result().as_str(),
            format!("devices/{uid}/cmd/reboot/result")
        );
        assert_eq!(t.client_id().as_str(), format!("devices-{uid}"));
    }

    #[test]
    fn longest_prefix_fits_client_id() {
        let t = Topics::new(&"p".repeat(PREFIX_MAX), &[0; 12]);
        assert_eq!(t.client_id().len(), 64);
    }

    #[test]
    fn commands_are_parsed_by_topic() {
        let t = Topics::new("d", &[1; 12]);
        let base = "d/010101010101010101010101";
        assert!(matches!(
            parse_command(&t, &format!("{base}/cmd/reboot")),
            Some(Command::Reboot)
        ));
        assert!(parse_command(&t, &format!("{base}/cmd/unknown")).is_none());
        assert!(parse_command(&t, "other/cmd/reboot").is_none());
        assert!(parse_command(&t, &format!("{base}/cmd/reboot/result")).is_none());
    }

    #[test]
    fn telemetry_is_json() {
        let mut s = std::string::String::new();
        write_telemetry(&mut s, 42).unwrap();
        assert_eq!(s, r#"{"uptime_s":42}"#);
    }
}
