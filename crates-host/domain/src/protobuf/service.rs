//! Обработчик сообщений хоста по умолчанию — над примером схемы
//! `crates-host/proto`.

use proto::{Request, Response};

use super::info;
use super::reply::{error, response};
use crate::device::DeviceInfo;
// {%- if ota == "true" %}
use core::array::TryFromSliceError;
use core::fmt::Write as _;

use defmt_or_log::warn;
use proto::{Error, Ota, Ota_, Response_, heapless};

use crate::ota::commands::{CHUNK_BYTES, Chunk, Rejection, SIGNATURE_BYTES};
use crate::ota::{Client, Command, Reply};
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
/// Каждая подсистема знает свои команды и обрабатывает их здесь же:
/// `InfoHandler` — сведения, `OtaHandler` — обновление.
/// `match` по командам генерирует `crates-host/proto/build.rs` из объявлений
/// `service` в `.proto`; здесь — только `Handlers` и ответ.
///
/// Команда, которой нужно что-то от платы, получает это полем — портом
/// (`ports::…`) над адаптером, а не железом: тогда её ответ проверяется на
/// хосте заглушкой порта.
#[derive(Debug)]
pub struct Service {
    handlers: proto::host::Handlers<
        info::InfoHandler,
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
        ota: Client,
        // {%- endif %}
    ) -> Self {
        Self {
            handlers: proto::host::Handlers {
                info: info::InfoHandler::new(info),
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

// {%- if ota == "true" %}
// Пределы полей схемы (`proto/build.rs`) — те же, что у команды узлу OTA:
// разойдутся — кусок или подпись, прошедшие разбор, не дойдут до узла.
const _: () = assert!(
    proto::OTA_CHUNK_BYTES == CHUNK_BYTES && proto::OTA_SIGNATURE_BYTES == SIGNATURE_BYTES,
    "пределы Ota.Chunk.data/Ota.Start.signature в proto/build.rs разошлись с ota::commandss"
);

/// Обработчик команд обновления.
#[derive(Debug)]
pub(crate) struct OtaHandler {
    /// Клиент узла OTA: вход `CMD` и номер транспорта.
    ota: Client,
}

impl OtaHandler {
    /// Обработчик, отдающий команды обновления узлу OTA клиентом `ota`.
    #[must_use]
    pub(crate) const fn new(ota: Client) -> Self {
        Self { ota }
    }

    /// Команда узлу OTA и её исход; узел OTA пропал — отказ устройства.
    async fn send(&self, command: Command) -> Response_::Result {
        let reply = self.ota.request(command).await.unwrap_or_else(|_lost| {
            warn!("protobuf: узел OTA не ответил");
            Err(Rejection::Device)
        });
        ota_reply(reply)
    }

    async fn start(&mut self, request: &Ota_::Start) -> Response_::Result {
        match signature(&request.signature) {
            Ok(signature) => {
                self.send(Command::Start {
                    len: request.len,
                    signature,
                })
                .await
            }
            Err(_) => error("signature must be 64 bytes"),
        }
    }

    async fn chunk(&mut self, request: &Ota_::Chunk) -> Response_::Result {
        match Chunk::new(request.offset, &request.data) {
            Some(chunk) => self.send(Command::Chunk(chunk)).await,
            // Ёмкость поля в схеме та же, что у куска: сюда не доходит.
            None => error("chunk too long"),
        }
    }

    async fn finish(&mut self) -> Response_::Result {
        self.send(Command::Finish).await
    }

    async fn reboot(&mut self) -> Response_::Result {
        self.send(Command::Reboot).await
    }
}

impl proto::host::HandleOta for OtaHandler {
    async fn ota(&mut self, request: &Ota) -> Response_::Result {
        match &request.command {
            Some(Ota_::Command::Start(request)) => self.start(request).await,
            Some(Ota_::Command::Chunk(request)) => self.chunk(request).await,
            Some(Ota_::Command::Finish(_)) => self.finish().await,
            Some(Ota_::Command::Reboot(_)) => self.reboot().await,
            None => error("unknown command"),
        }
    }
}

/// Подпись из `start`: пусто — подписи нет (узел OTA отвергнет `Start`),
/// 64 байта — подпись, иначе отказ: обрезанную подпись принять за целую нельзя.
fn signature(bytes: &[u8]) -> Result<Option<[u8; SIGNATURE_BYTES]>, TryFromSliceError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    bytes.try_into().map(Some)
}

/// Исход команды обновления — `ok` или `error` с именем отказа
/// ([`Rejection`]).
fn ota_reply(reply: Reply) -> Response_::Result {
    match reply {
        Ok(_) => Response_::Result::Ok(proto::Ok {}),
        Err(rejection) => {
            let mut message = heapless::String::new();
            // Имя варианта короче `ERROR_TEXT_BYTES`; не влезло бы — пустой
            // текст, как у `text`.
            if write!(message, "{rejection:?}").is_err() {
                message.clear();
            }
            Response_::Result::Error(Error { text: message })
        }
    }
}
// {%- endif %}

#[cfg(test)]
mod tests {
    use core::task::{Context, Poll, Waker};

    use proto::{Get, Request_, Response_, heapless};

    use super::*;
    use crate::protobuf::info::INFO;
    // {%- if ota == "true" %}
    use crate::ota::commands::Progress;
    // {%- endif %}

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

    /// Подпись не той длины — ответ-отказ сразу, до узла OTA она не доходит.
    #[test]
    fn a_signature_is_empty_or_exactly_64_bytes() {
        assert!(signature(&[]).unwrap().is_none());
        assert!(signature(&[0; SIGNATURE_BYTES]).unwrap().is_some());
        assert!(signature(&[0; 8]).is_err());
    }

    /// Исход — `ok` или имя отказа.
    #[test]
    fn an_outcome_becomes_ok_or_the_rejection_name() {
        assert!(matches!(
            ota_reply(Ok(Progress::Started)),
            Response_::Result::Ok(_)
        ));
        let refused = ota_reply(Err(Rejection::NoSession));
        assert!(
            matches!(&refused, Response_::Result::Error(e) if e.text == "NoSession"),
            "{refused:?}"
        );
    }
    // {%- endif %}
}
