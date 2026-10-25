//! Имена тем и идентификатор клиента платы, разбор команд по теме.

use heapless::String;

use crate::device::UID_BYTES;

/// Самый длинный префикс тем: вместе с `-` и номером чипа (24 hex) он даёт
/// 64 байта — предел идентификатора клиента.
pub const PREFIX_MAX: usize = 39;

/// Идентификатор клиента: `<prefix>-<uid>`.
pub(super) const CLIENT_ID_LEN: usize = PREFIX_MAX + 1 + UID_HEX;
pub(super) const REBOOT: &str = "/cmd/reboot";
/// `<prefix>/<uid>` и самая длинная тема под ним (`/cmd/reboot/result`).
pub(super) const TOPIC_LEN: usize = CLIENT_ID_LEN + "/cmd/reboot/result".len();

/// Hex-знаков в номере чипа.
const UID_HEX: usize = 2 * UID_BYTES;

/// Команда с брокера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Перезагрузить МК.
    Reboot,
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

    pub(super) fn topic<const N: usize>(&self, suffix: &str) -> String<N> {
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
