//! Обработчик сообщений хоста по умолчанию — над примером схемы
//! `crates-host/proto`.

use ports::HostService;
use proto::{Error, Info, Pong, Request, Request_, Response, Response_, heapless};

/// Обработчик, с которым узел `HOST` стоит в графе шаблона: на `ping` —
/// `pong`, на `get_info` — версия прошивки, `request_id` — обратно.
///
/// Это отправная точка, а не готовая логика: проект меняет схему в
/// `crates-host/proto` и команды здесь — перевод сообщений схемы в свои
/// проверенные типы и ответы на них. Версия приходит снаружи, потому что её
/// знает прошивка (`app`), а не `domain`.
#[derive(Debug)]
pub struct Service {
    version: &'static str,
}

impl Service {
    /// Обработчик, отвечающий на `get_info` версией `version`.
    ///
    /// # Panics
    ///
    /// Если `version` длиннее `proto::VERSION_BYTES`: обрезанная версия
    /// выглядела бы как настоящая, а пустую строку ответ не заслуживает. В
    /// `const`-контексте — ошибка компиляции.
    #[must_use]
    pub const fn new(version: &'static str) -> Self {
        assert!(
            version.len() <= proto::VERSION_BYTES,
            "версия не помещается в Info.version"
        );
        Self { version }
    }
}

impl HostService for Service {
    type Request = Request;
    type Response = Response;

    fn handle(&mut self, request: Request) -> impl Future<Output = Response> {
        let result = match request.command {
            Some(Request_::Command::Ping(_)) => Response_::Result::Pong(Pong::default()),
            Some(Request_::Command::GetInfo(_)) => Response_::Result::Info(Info {
                version: text(self.version),
            }),
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
/// `proto`) — пустая: обрезанная строка выглядела бы как настоящая. Версию
/// длиннее предела отсекает [`Service::new`], так что это касается только
/// текстов ошибок.
fn text<const N: usize>(s: &str) -> heapless::String<N> {
    heapless::String::try_from(s).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_version_of_exactly_the_limit_is_accepted() {
        let version: &'static str = "v".repeat(proto::VERSION_BYTES).leak();
        drop(Service::new(version));
    }

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_version_over_the_limit_is_refused() {
        let version: &'static str = "v".repeat(proto::VERSION_BYTES + 1).leak();
        drop(Service::new(version));
    }
}
