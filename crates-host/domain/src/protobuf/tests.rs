//! Тесты узла `PROTOBUF`: гоняют узел над схемой `crates-host/proto`.

use core::future::poll_fn;
use core::pin::{Pin, pin};
use core::task::{Context, Poll, Waker};
use std::collections::VecDeque;

use micropb::{MessageDecode, MessageEncode, PbEncoder};
use ports::{FrameDuplex, FrameLink, LinkError};
use proto::{Get, Request, Request_, Response, Response_, heapless};
use supervisor::runtime::TaskExit;

use super::service::tests::{INFO, service};
// {%- if ota == "true" %}
use super::Service;
// {%- endif %}
use super::task;

/// Канал: отдаёт заготовленные исходы `recv` — кадр снимается в опросе,
/// который его возвращает, как требует `FrameDuplex`; кончились — ждёт.
/// Отправленные кадры складывает в `sent`.
struct Link {
    incoming: VecDeque<Result<Vec<u8>, LinkError<&'static str>>>,
    sent: Vec<Vec<u8>>,
    buffer: Vec<u8>,
}

impl Link {
    fn with(incoming: impl IntoIterator<Item = Result<Vec<u8>, LinkError<&'static str>>>) -> Self {
        Self {
            incoming: incoming.into_iter().collect(),
            sent: Vec::new(),
            buffer: vec![0; 128],
        }
    }
}

impl FrameLink for Link {
    type Error = &'static str;

    fn recv(&mut self) -> impl Future<Output = Result<usize, LinkError<Self::Error>>> {
        poll_fn(|_| match self.incoming.pop_front() {
            Some(next) => Poll::Ready(next.map(|frame| {
                self.buffer[..frame.len()].copy_from_slice(&frame);
                frame.len()
            })),
            None => Poll::Pending,
        })
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.buffer
    }

    fn send(&mut self, len: usize) -> impl Future<Output = Result<(), LinkError<Self::Error>>> {
        self.sent.push(self.buffer[..len].to_vec());
        std::future::ready(Ok(()))
    }
}

impl FrameDuplex for Link {
    fn send_from(
        &mut self,
        frame: &[u8],
    ) -> impl Future<Output = Result<(), LinkError<Self::Error>>> {
        self.sent.push(frame.to_vec());
        std::future::ready(Ok(()))
    }
}

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

fn poll(node: Pin<&mut (impl Future<Output = TaskExit> + ?Sized)>) {
    let mut cx = Context::from_waker(Waker::noop());
    assert!(node.poll(&mut cx).is_pending(), "узел не выходит");
}

/// Гоняет узел над `link` с буфером ответа на `capacity` байт по всем
/// заготовленным кадрам, пока он не станет ждать следующий.
fn serve(link: &mut Link, capacity: usize) {
    let stand = supervisor::stand!(PROTOBUF_FRAG<Link>);
    let mut service = service(INFO);
    let response: &'static mut [u8] = vec![0; capacity].leak();
    let ctx = stand.ctx(link, &mut service, response);
    poll(pin!(task(ctx)));
}

#[test]
fn answers_requests_with_their_request_id() {
    let mut link = Link::with([
        Ok(request(7, Request_::Command::Info(Get::default()))),
        Ok(request(8, Request_::Command::Info(Get::default()))),
    ]);
    serve(&mut link, 64);
    let replies: Vec<Response> = link.sent.iter().map(|f| decode(f)).collect();
    assert_eq!(replies[0].request_id, 7);
    assert_eq!(replies[1].request_id, 8);
    assert!(matches!(&replies[1].result, Some(Response_::Result::Info(i)) if i.version == "1.2.3"));
}

#[test]
fn malformed_frame_gets_an_error_and_lost_frame_is_skipped() {
    let mut link = Link::with([
        Ok(vec![0xFF, 0xFF, 0xFF]),
        Err(LinkError::Lost("помеха")),
        Ok(request(9, Request_::Command::Info(Get::default()))),
    ]);
    serve(&mut link, 64);
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
    let mut link = Link::with([
        Ok(request(1, Request_::Command::Info(Get::default()))),
        Ok(encode(&Request {
            request_id: 2,
            command: None,
        })),
    ]);
    // Ошибка «unknown command» с id влезает в 32 байта, `info` — нет.
    serve(&mut link, 32);
    assert_eq!(link.sent.len(), 1);
    assert_eq!(decode(&link.sent[0]).request_id, 2);
}

/// Запрос без команды — тоже запрос: ответ-ошибка с тем же `request_id`.
#[test]
fn a_request_without_a_command_gets_an_error_reply() {
    let mut link = Link::with([Ok(encode(&Request {
        request_id: 4,
        command: None,
    }))]);
    serve(&mut link, 64);
    assert_eq!(link.sent.len(), 1);
    let reply = decode(&link.sent[0]);
    assert_eq!(reply.request_id, 4);
    assert!(matches!(reply.result, Some(Response_::Result::Error(_))));
}
// {%- if ota == "true" %}

/// Команда обновления уходит узлу OTA на вход `CMD`, и ответ хосту — её
/// исход с номером запроса.
#[test]
fn an_update_command_is_answered_with_its_outcome() {
    let mut link = Link::with([Ok(request(
        1,
        Request_::Command::OtaStart(proto::OtaStart {
            len: 8,
            signature: heapless::Vec::new(),
        }),
    ))]);
    {
        let stand = supervisor::stand!(PROTOBUF_FRAG<Link>);
        let (ota, mut ota_node) = crate::ota::tests::live();
        let mut service = Service::new(INFO, ota);
        let response: &'static mut [u8] = vec![0; 64].leak();
        let mut node = pin!(task(stand.ctx(&mut link, &mut service, response)));
        for _ in 0..8 {
            poll(node.as_mut());
            poll(ota_node.as_mut());
        }
    }
    let replies: Vec<Response> = link.sent.iter().map(|f| decode(f)).collect();
    assert_eq!(replies.len(), 1, "{replies:?}");
    assert_eq!(replies[0].request_id, 1);
    assert!(
        matches!(replies[0].result, Some(Response_::Result::Ok(_))),
        "{replies:?}"
    );
}
// {%- endif %}

proptest::proptest! {
    /// Кадр приходит снаружи, и каким бы он ни был — мусор, обрезок, чужая
    /// схема, — узел не падает, отвечает на него не больше чем одним кадром
    /// (команду обновления узел отдаёт OTA, и её исход приходит позже), и этот
    /// кадр — ответ схемы.
    #[test]
    fn any_frame_gets_at_most_one_well_formed_reply(
        frame in proptest::collection::vec(proptest::num::u8::ANY, 0..128)
    ) {
        let mut link = Link::with([Ok(frame)]);
        serve(&mut link, 256);
        proptest::prop_assert!(link.sent.len() <= 1, "{} ответов", link.sent.len());
        if let Some(reply) = link.sent.first() {
            let mut response = Response::default();
            proptest::prop_assert!(response.decode_from_bytes(reply).is_ok());
        }
    }
}
