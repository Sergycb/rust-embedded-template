//! Обработчик сообщений хоста по умолчанию — над примером схемы
//! `crates-host/proto`.

use ports::HostService;
use proto::{Error, Info, Pong, Request, Request_, Response, Response_, heapless};

/// Сведения об устройстве, которыми обработчик отвечает на `info`.
///
/// Приходят снаружи: версию и коммит знает прошивка (`app`), чип — плата
/// (`bsp::ChipSpec`), а не `domain`.
#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    /// Версия прошивки, не длиннее `proto::VERSION_BYTES`.
    pub version: &'static str,
    /// Короткий хеш коммита, не длиннее `proto::COMMIT_BYTES`.
    pub commit: &'static str,
    /// Имя чипа, не длиннее `proto::CHIP_BYTES`.
    pub chip: &'static str,
    /// Заводской номер экземпляра чипа.
    pub uid: [u8; proto::UID_BYTES],
    /// Объём flash чипа, байт.
    pub flash_size: u32,
    /// Частота ядра, Гц; `None` — неизвестна.
    pub sys_hz: Option<u32>,
}

/// Обработчик, с которым узел `HOST` стоит в графе шаблона: на `ping` —
/// `pong`, на `info` — сведения об устройстве, `request_id` — обратно.
///
/// Это отправная точка, а не готовая логика: проект меняет схему в
/// `crates-host/proto` и команды здесь — перевод сообщений схемы в свои
/// проверенные типы и ответы на них.
#[derive(Debug)]
pub struct Service {
    info: DeviceInfo,
}

impl Service {
    /// Обработчик, отвечающий на `info` сведениями `info`.
    ///
    /// # Panics
    ///
    /// Если строка `info` длиннее ёмкости своего поля в схеме (`proto::*_BYTES`):
    /// обрезанная выглядела бы как настоящая, а пустую ответ не заслуживает. В
    /// `const`-контексте — ошибка компиляции.
    #[must_use]
    pub const fn new(info: DeviceInfo) -> Self {
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
        Self { info }
    }

    fn info(&self) -> Info {
        let info = &self.info;
        Info {
            version: text(info.version),
            commit: text(info.commit),
            chip: text(info.chip),
            // Ёмкость поля — `UID_BYTES`, длина массива — та же: не откажет.
            uid: heapless::Vec::from_slice(&info.uid).unwrap_or_default(),
            flash_size: info.flash_size,
            sys_hz: info.sys_hz.unwrap_or(0),
        }
    }
}

impl HostService for Service {
    type Request = Request;
    type Response = Response;

    fn handle(&mut self, request: Request) -> impl Future<Output = Response> {
        let result = match request.command {
            Some(Request_::Command::Ping(_)) => Response_::Result::Pong(Pong::default()),
            Some(Request_::Command::Info(_)) => Response_::Result::Info(self.info()),
            None => Response_::Result::Error(Error {
                text: text("unknown command"),
            }),
        };
        core::future::ready(Response {
            request_id: request.request_id,
            result: Some(result),
        })
    }

    fn malformed(&mut self) -> Response {
        Response {
            request_id: 0,
            result: Some(Response_::Result::Error(Error {
                text: text("bad frame"),
            })),
        }
    }
}

/// Строка схемы из `s`; не помещается в предел поля (`build.rs` крейта
/// `proto`) — пустая: обрезанная строка выглядела бы как настоящая. Строки
/// [`DeviceInfo`] длиннее предела отсекает [`Service::new`], так что это
/// касается только текстов ошибок.
fn text<const N: usize>(s: &str) -> heapless::String<N> {
    heapless::String::try_from(s).unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Сведения, которыми тесты собирают обработчик.
    pub(crate) const INFO: DeviceInfo = DeviceInfo {
        version: "1.2.3",
        commit: "abcdef12",
        chip: "stm32f407ve",
        uid: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        flash_size: 512 * 1024,
        sys_hz: None,
    };

    #[test]
    fn info_carries_every_field() {
        let info = Service::new(INFO).info();
        assert_eq!(info.version, "1.2.3");
        assert_eq!(info.commit, "abcdef12");
        assert_eq!(info.chip, "stm32f407ve");
        assert_eq!(info.uid.as_slice(), &INFO.uid);
        assert_eq!(info.flash_size, 512 * 1024);
        assert_eq!(info.sys_hz, 0, "неизвестная частота — 0");
    }

    #[test]
    fn a_version_of_exactly_the_limit_is_accepted() {
        let version: &'static str = "v".repeat(proto::VERSION_BYTES).leak();
        drop(Service::new(DeviceInfo { version, ..INFO }));
    }

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_version_over_the_limit_is_refused() {
        let version: &'static str = "v".repeat(proto::VERSION_BYTES + 1).leak();
        drop(Service::new(DeviceInfo { version, ..INFO }));
    }

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_chip_name_over_the_limit_is_refused() {
        let chip: &'static str = "c".repeat(proto::CHIP_BYTES + 1).leak();
        drop(Service::new(DeviceInfo { chip, ..INFO }));
    }
}
