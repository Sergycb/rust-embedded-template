//! Обработчик `InfoService`: сведения об устройстве.

use embassy_time::Instant;
use proto::{Get, Info, Response_, heapless};

use super::reply::text;
use crate::device::DeviceInfo;

// Поле `uid` схемы — ровно заводской номер: копируется без проверки длины.
const _: () = assert!(proto::UID_BYTES == crate::device::UID_BYTES);
// Прошлую панику `app` сжимает до `PANIC_BRIEF` — она помещается в поле.
const _: () = assert!(crate::device::PANIC_BRIEF <= proto::PANIC_BYTES);

/// Обработчик команды `info`.
#[derive(Debug)]
pub(crate) struct Handler {
    info: DeviceInfo,
}

impl Handler {
    /// Обработчик, отвечающий на `info` сведениями `info`.
    ///
    /// # Panics
    ///
    /// Если строка `info` длиннее ёмкости своего поля в схеме (`proto::*_BYTES`):
    /// обрезанная выглядела бы как настоящая, а пустую ответ не заслуживает. В
    /// `const`-контексте — ошибка компиляции.
    #[must_use]
    pub(crate) const fn new(info: DeviceInfo) -> Self {
        assert!(
            info.version.len() <= proto::VERSION_BYTES,
            "версия не помещается в Info.version"
        );
        assert!(
            info.commit.len() <= proto::COMMIT_BYTES,
            "коммит не помещается в Info.commit"
        );
        assert!(
            info.chip.len() <= proto::CHIP_BYTES,
            "имя чипа не помещается в Info.chip"
        );
        assert!(
            info.reset.len() <= proto::RESET_BYTES,
            "причина сброса не помещается в Info.reset"
        );
        if let Some(panic) = info.last_panic {
            assert!(
                panic.len() <= proto::PANIC_BYTES,
                "паника не помещается в Info.last_panic"
            );
        }
        Self { info }
    }

    fn message(&self) -> Info {
        let info = &self.info;
        Info {
            version: text(info.version),
            commit: text(info.commit),
            chip: text(info.chip),
            // Ёмкость поля — `UID_BYTES`, длина массива — та же: не откажет.
            uid: heapless::Vec::from_slice(&info.uid).unwrap_or_default(),
            flash_size: info.flash_size,
            sys_hz: info.sys_hz.unwrap_or(0),
            // Часы `embassy-time` идут с запуска: их отсчёт и есть время работы.
            // Секунды в `u32` — 136 лет; 64-битные поля `micropb` кодирует
            // только с фичей `enable-64bit`.
            uptime_s: u32::try_from(Instant::now().as_secs()).unwrap_or(u32::MAX),
            reset: text(info.reset),
            last_panic: text(info.last_panic.unwrap_or("")),
        }
    }
}

impl proto::host::HandleInfo for Handler {
    // Команда не ждёт задачу, но трейт общий с теми, что ждут.
    #[allow(
        clippy::unused_async_trait_impl,
        reason = "команда не ждёт задачу, но трейт общий с теми, что ждут"
    )]
    async fn info(&mut self, _request: &Get) -> Response_::Result {
        Response_::Result::Info(self.message())
    }
}

#[cfg(test)]
pub(crate) const INFO: DeviceInfo = DeviceInfo {
    version: "1.2.3",
    commit: "abcdef12",
    chip: "stm32f407ve",
    uid: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
    flash_size: 512 * 1024,
    sys_hz: None,
    reset: "pin",
    last_panic: None,
};

#[cfg(test)]
mod tests {
    use super::*;

    fn handler(info: DeviceInfo) -> Handler {
        Handler::new(info)
    }

    #[test]
    fn info_carries_every_field() {
        let info = handler(INFO).message();
        assert_eq!(info.version, "1.2.3");
        assert_eq!(info.commit, "abcdef12");
        assert_eq!(info.chip, "stm32f407ve");
        assert_eq!(info.uid.as_slice(), &INFO.uid);
        assert_eq!(info.flash_size, 512 * 1024);
        assert_eq!(info.sys_hz, 0, "неизвестная частота — 0");
        assert_eq!(info.reset, "pin");
        assert_eq!(info.last_panic, "", "паники не было — пусто");
        let info = handler(DeviceInfo {
            last_panic: Some("main.rs:7:1:\nboom"),
            ..INFO
        })
        .message();
        assert_eq!(info.last_panic, "main.rs:7:1:\nboom");
    }

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_reset_cause_over_the_limit_is_refused() {
        let reset: &'static str = "r".repeat(proto::RESET_BYTES + 1).leak();
        let _handler = handler(DeviceInfo { reset, ..INFO });
    }

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_panic_over_the_limit_is_refused() {
        let panic: &'static str = "p".repeat(proto::PANIC_BYTES + 1).leak();
        let _handler = handler(DeviceInfo {
            last_panic: Some(panic),
            ..INFO
        });
    }

    #[test]
    fn info_reports_uptime() {
        let before = handler(INFO).message().uptime_s;
        embassy_time::MockDriver::get().advance(embassy_time::Duration::from_secs(2));
        assert!(handler(INFO).message().uptime_s >= before + 2);
    }

    /// Версия ровно в предел поля уходит целиком, а не обрезанной.
    #[test]
    fn a_version_of_exactly_the_limit_is_accepted() {
        let version: &'static str = "v".repeat(proto::VERSION_BYTES).leak();
        let info = handler(DeviceInfo { version, ..INFO }).message();
        assert_eq!(info.version, version);
    }

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_version_over_the_limit_is_refused() {
        let version: &'static str = "v".repeat(proto::VERSION_BYTES + 1).leak();
        let _handler = handler(DeviceInfo { version, ..INFO });
    }

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_chip_name_over_the_limit_is_refused() {
        let chip: &'static str = "c".repeat(proto::CHIP_BYTES + 1).leak();
        let _handler = handler(DeviceInfo { chip, ..INFO });
    }
}
