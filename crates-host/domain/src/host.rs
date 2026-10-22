//! Сообщения protobuf от хоста: обработчик кадра [`Protobuf`](crate::host::Protobuf) для узла
//! протокола `domain::server` (в графе — узел `HOST_<транспорт>`).
//!
//! Единственное место, где сообщения схемы превращаются в типы приложения,
//! — обработчик [`HostService`](ports::HostService) приложения: в задачи
//! уходят проверенные значения своих типов. [`Protobuf`](crate::host::Protobuf) только разбирает
//! запрос из кадра и кодирует ответ в него же. Схему он не знает: сообщения —
//! параметры (`micropb::MessageDecode`/`MessageEncode`), пример схемы — крейт
//! `crates-host/proto`.
//!
//! Канал под ним — любой [`FrameLink`](ports::FrameLink), кадр — одно
//! сообщение целиком: датаграммы (UDP) дают его сами, поток (UART, USB CDC,
//! TCP) — через `adapters::cobs::Cobs`.
//!
//! Что обработчик делает с кадром:
//!
//! * кадр не разобрался как запрос — ответ
//!   [`HostService::malformed`](ports::HostService::malformed);
//! * ответ не помещается в буфер — в лог и без ответа, хост получит таймаут;
//!   что буфер мал для какого-то ответа схемы, обработчик предупреждает уже
//!   при старте узла.
//!
//! Сообщения по инициативе устройства (события, телеметрия) — второй вход
//! узла; появятся по вопросу при генерации проекта.

// Обработчик по умолчанию над схемой `crates-host/proto` — только в проекте с
// протоколом protobuf: без него крейта схемы в проекте нет вовсе.
/// Обработчик по умолчанию над примером схемы `crates-host/proto`.
mod service;

pub use service::{DeviceInfo, Service};

use defmt_or_log::warn;
use micropb::{DecodeError, MessageDecode, MessageEncode, PbEncoder};
use ports::{Handler, HostService};

/// Обработчик кадров protobuf над обработчиком сообщений приложения `S`:
/// разбирает запрос, отдаёт его `S`, кодирует ответ.
///
/// Обёртка, а не реализация [`Handler`] для всякого [`HostService`]: оба
/// трейта чужие для `domain`, и общая реализация была бы возможна только в
/// `ports`, которому `micropb` знать незачем.
#[derive(Debug)]
pub struct Protobuf<S>(pub S);

impl<S> Handler for Protobuf<S>
where
    S: HostService,
    S::Request: MessageDecode + Default,
    S::Response: MessageEncode,
{
    fn begin(&mut self, capacity: usize) {
        if let Ok(max) = S::Response::MAX_SIZE
            && capacity < max
        {
            warn!(
                "host: буфер канала {} байт, а самый длинный ответ схемы — {}: такие ответы не уйдут",
                capacity, max
            );
        }
    }

    async fn handle(&mut self, frame: &mut [u8], len: usize) -> Option<usize> {
        let mut request = S::Request::default();
        let response = match request.decode_from_bytes(&frame[..len]) {
            Ok(()) => self.0.handle(request).await,
            Err(err) => {
                warn!("host: кадр не разобрался: {}", decode_reason(&err));
                self.0.malformed()
            }
        };
        let reply = encode(&response, frame);
        if reply.is_none() {
            warn!("host: ответ не помещается в буфер на {} байт", frame.len());
        }
        reply
    }
}

/// Почему кадр не разобрался — строкой: у `micropb::DecodeError` нет
/// `defmt::Format`, а `Debug2Format` втянул бы в прошивку `core::fmt` ради
/// одной строки лога.
fn decode_reason<E>(err: &DecodeError<E>) -> &'static str {
    match err {
        DecodeError::VarIntLimit => "varint длиннее 10 байт",
        DecodeError::UnexpectedEof => "кадр оборвался",
        DecodeError::Deprecation => "устаревший тип поля",
        DecodeError::UnknownWireType => "неизвестный тип поля",
        DecodeError::ZeroField => "поле с номером 0",
        DecodeError::CustomField => "поле не разобралось",
        DecodeError::Utf8 => "строка не UTF-8",
        DecodeError::Capacity => "поле не помещается в контейнер",
        DecodeError::WrongLen => "длина поля не сходится",
        DecodeError::Reader(_) => "ошибка чтения",
        // `DecodeError` — `#[non_exhaustive]`: новая причина в `micropb`.
        _ => "кадр не разобрался",
    }
}

/// Кодирует `message` в начало `buf`; `None` — не поместилось.
fn encode(message: &impl MessageEncode, buf: &mut [u8]) -> Option<usize> {
    let capacity = buf.len();
    let mut encoder = PbEncoder::new(buf);
    message.encode(&mut encoder).ok()?;
    Some(capacity - encoder.into_writer().len())
}

// Тесты гоняют обработчик над схемой `crates-host/proto` через узел протокола.
#[cfg(test)]
mod tests {
    use micropb::{MessageDecode, MessageEncode, PbEncoder};
    use ports::LinkError;
    use proto::{Get, Request, Request_, Response, Response_, heapless};

    use super::Protobuf;
    use super::service::tests::{INFO, service};
    use crate::server::fakes::Link;

    fn encode(message: &impl MessageEncode) -> Vec<u8> {
        let mut encoder = PbEncoder::new(heapless::Vec::<u8, 128>::new());
        message.encode(&mut encoder).unwrap();
        encoder.into_writer().to_vec()
    }

    fn decode(frame: &[u8]) -> Response {
        let mut response = Response::default();
        response.decode_from_bytes(frame).unwrap();
        response
    }

    fn request(id: u32, command: Request_::Command) -> Vec<u8> {
        encode(&Request {
            request_id: id,
            command: Some(command),
        })
    }

    /// Гоняет узел протокола с обработчиком protobuf по всем заготовленным
    /// кадрам, пока он не станет ждать следующий.
    fn serve(link: &mut Link) {
        crate::server::fakes::serve(link, &mut Protobuf(service(INFO)));
    }

    #[test]
    fn answers_requests_with_their_request_id() {
        let mut link = Link::with(
            [
                Ok(request(7, Request_::Command::Info(Get::default()))),
                Ok(request(8, Request_::Command::Info(Get::default()))),
            ],
            64,
        );
        serve(&mut link);
        let replies: Vec<Response> = link.sent.iter().map(|f| decode(f)).collect();
        assert_eq!(replies[0].request_id, 7);
        assert_eq!(replies[1].request_id, 8);
        assert!(
            matches!(&replies[1].result, Some(Response_::Result::Info(i)) if i.version == "1.2.3")
        );
    }

    #[test]
    fn malformed_frame_gets_an_error_and_lost_frame_is_skipped() {
        let mut link = Link::with(
            [
                Ok(vec![0xFF, 0xFF, 0xFF]),
                Err(LinkError::Lost("помеха")),
                Ok(request(9, Request_::Command::Info(Get::default()))),
            ],
            64,
        );
        serve(&mut link);
        assert_eq!(link.sent.len(), 2, "на потерянный кадр ответа нет");
        let malformed = decode(&link.sent[0]);
        assert_eq!(malformed.request_id, 0);
        assert!(matches!(
            malformed.result,
            Some(Response_::Result::Error(_))
        ));
        assert_eq!(decode(&link.sent[1]).request_id, 9);
    }

    #[test]
    fn reply_that_does_not_fit_is_dropped_not_truncated() {
        let mut link = Link::with(
            [
                Ok(request(1, Request_::Command::Info(Get::default()))),
                Ok(encode(&Request {
                    request_id: 2,
                    command: None,
                })),
            ],
            32,
        );
        // Ошибка «unknown command» с id влезает в 32 байта, `info` — нет.
        serve(&mut link);
        assert_eq!(link.sent.len(), 1);
        assert_eq!(decode(&link.sent[0]).request_id, 2);
    }

    /// Запрос без команды — тоже запрос: ответ-ошибка с тем же `request_id`.
    #[test]
    fn a_request_without_a_command_gets_an_error_reply() {
        let mut link = Link::with(
            [Ok(encode(&Request {
                request_id: 4,
                command: None,
            }))],
            64,
        );
        serve(&mut link);
        assert_eq!(link.sent.len(), 1);
        let reply = decode(&link.sent[0]);
        assert_eq!(reply.request_id, 4);
        assert!(matches!(reply.result, Some(Response_::Result::Error(_))));
    }
}
