//! Обработчик сообщений хоста по умолчанию — над примером схемы
//! `crates-host/proto`.

use proto::{Request, Response};

use super::info;
use super::reply::{error, response};
use crate::device::DeviceInfo;
// {%- if ota == "true" %}
use crate::ota::OtaClient;
use crate::ota::protobuf::Handler as OtaHandler;
// {%- endif %}

/// Самый длинный запрос схемы, байт: под него `app` заводит буфер канала
/// узла (привязка узла в графе или задача протокола в `crates-cross/app`).
/// Предел выводит `micropb` из ёмкостей полей (`crates-host/proto/build.rs`),
/// поэтому буфер растёт вместе со схемой.
pub const REQUEST_MAX: usize = match <Request as micropb::MessageEncode>::MAX_SIZE {
    Ok(max) => max,
    Err(_) => panic!("у запроса схемы нет предела длины"),
};

/// Самый длинный ответ схемы, байт: под него `app` заводит буфер ответа узла.
pub const RESPONSE_MAX: usize = match <Response as micropb::MessageEncode>::MAX_SIZE {
    Ok(max) => max,
    Err(_) => panic!("у ответа схемы нет предела длины"),
};

/// Обработчик сообщений узла `PROTOBUF`: общий `Handlers` схемы разводит
/// запрос по обработчикам подсистем, а `request_id` возвращается в ответе.
///
/// Каждая подсистема знает свои команды и обрабатывает их в своём модуле
/// (`crate::protobuf::info`{% if ota == "true" %}, `crate::ota::protobuf`{% endif %}).
/// `match` по командам генерирует `crates-host/proto/build.rs` из объявлений
/// `service` в `.proto`; здесь — только `Handlers` и ответ.
///
/// Команда, которой нужно что-то от платы, получает это полем — портом
/// (`ports::…`) над адаптером, а не железом: тогда её ответ проверяется на
/// хосте заглушкой порта.
#[derive(Debug)]
pub struct Service {
    handlers: proto::host::Handlers<
        info::Handler,
        // {%- if ota == "true" %}
        OtaHandler,
        // {%- endif %}
    >,
}

impl Service {
    /// Обработчик, отвечающий на `info` сведениями `info`{% if ota == "true" %}, а команды
    /// обновления отдающий узлу OTA клиентом `ota`{% endif %}.
    ///
    /// # Panics
    ///
    /// Если строка `info` длиннее ёмкости своего поля в схеме (`proto::*_BYTES`):
    /// обрезанная выглядела бы как настоящая, а пустую ответ не заслуживает. В
    /// `const`-контексте — ошибка компиляции.
    #[must_use]
    pub const fn new(
        info: DeviceInfo,
        // {%- if ota == "true" %}
        ota: OtaClient,
        // {%- endif %}
    ) -> Self {
        Self {
            handlers: proto::host::Handlers {
                info: info::Handler::new(info),
                // {%- if ota == "true" %}
                ota: OtaHandler::new(ota),
                // {%- endif %}
            },
        }
    }

    /// Выполняет запрос и строит ответ с его `request_id`.
    pub async fn handle(&mut self, request: Request) -> Response {
        let request_id = request.request_id;
        let result = self
            .handlers
            .handle(&request)
            .await
            .unwrap_or_else(|| error("unknown command"));
        response(request_id, result)
    }

    /// Ответ на кадр, который не разобрался как запрос: битые байты, строка
    /// длиннее ёмкости, чужое сообщение; `request_id` взять неоткуда — 0.
    #[must_use]
    pub fn malformed(&self) -> Response {
        response(0, error("bad frame"))
    }
}

#[cfg(test)]
mod tests {
    use core::task::{Context, Poll, Waker};

    use proto::{Get, Request_, Response_, heapless};

    use super::*;
    use crate::protobuf::info::INFO;

    /// Обработчик со сведениями `info`{% if ota == "true" %}; клиент OTA — к узлу, которого
    /// никто не опрашивает (команды обновления гоняют тесты узлов){% endif %}.
    fn service(info: DeviceInfo) -> Service {
        Service::new(
            info,
            // {%- if ota == "true" %}
            crate::ota::tests::live().0,
            // {%- endif %}
        )
    }

    /// Ответ на запрос, который не ждёт задачу.
    fn reply(mut service: Service, request: Request) -> Response {
        let mut handled = core::pin::pin!(service.handle(request));
        let mut cx = Context::from_waker(Waker::noop());
        match handled.as_mut().poll(&mut cx) {
            Poll::Ready(reply) => reply,
            Poll::Pending => panic!("ответ без ожидания"),
        }
    }

    #[test]
    fn info_is_routed_to_the_info_handler() {
        let reply = reply(
            service(INFO),
            Request {
                request_id: 7,
                command: Some(Request_::Command::Info(Get::default())),
            },
        );
        assert_eq!(reply.request_id, 7);
        assert!(matches!(reply.result, Some(Response_::Result::Info(_))));
    }

    #[test]
    fn an_unknown_command_gets_an_error() {
        let reply = reply(
            service(INFO),
            Request {
                request_id: 4,
                command: None,
            },
        );
        assert_eq!(reply.request_id, 4);
        assert!(matches!(reply.result, Some(Response_::Result::Error(_))));
    }

    // {%- if ota == "true" %}
    /// Подпись не той длины — ответ-отказ сразу, до узла OTA она не доходит:
    /// узла здесь никто не опрашивает, и ответ готов с первого опроса.
    #[test]
    fn a_signature_of_the_wrong_length_is_refused() {
        let reply = reply(
            service(INFO),
            Request {
                request_id: 7,
                command: Some(Request_::Command::Ota(proto::Ota {
                    command: Some(proto::Ota_::Command::Start(proto::Ota_::Start {
                        len: 8,
                        signature: heapless::Vec::from_slice(&[0; 8]).unwrap(),
                    })),
                })),
            },
        );
        assert_eq!(reply.request_id, 7);
        assert!(matches!(reply.result, Some(Response_::Result::Error(_))));
    }
    // {%- endif %}
}
