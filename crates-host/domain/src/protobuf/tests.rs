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
use super::{Service, task};

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
    let stand = supervisor::stand!(PROTOBUF_FRAG<Link, Service>);
    let mut service = service(INFO);
    let response: &'static mut [u8] = vec![0; capacity].leak();
    // {%- if ota == "true" %}
    let mut ota_client = crate::ota::ClientId(0);
    // {%- endif %}
    let ctx = stand.ctx(
        link,
        &mut service,
        response,
        // {%- if ota == "true" %}
        &mut ota_client,
        // {%- endif %}
    );
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

/// Команда обновления уходит узлу OTA без ожидания: на следующий запрос
/// узел отвечает, пока узел OTA молчит, а исход приходит позже — ответом
/// с номером своего запроса.
#[test]
fn an_update_command_does_not_hold_back_other_requests() {
    let mut link = Link::with([
        Ok(request(
            1,
            Request_::Command::OtaStart(proto::OtaStart {
                len: 8,
                signature: heapless::Vec::new(),
            }),
        )),
        Ok(request(2, Request_::Command::Info(Get::default()))),
    ]);
    {
        let stand = supervisor::stand!(PROTOBUF_FRAG<Link, Service>);
        let mut ota_node =
            crate::ota::tests::serving(stand.ota_ask.server().expect("сервер ASK свободен"));
        let mut service = service(INFO);
        let response: &'static mut [u8] = vec![0; 64].leak();
        let mut ota_client = crate::ota::ClientId(0);
        let ctx = stand.ctx(&mut link, &mut service, response, &mut ota_client);
        let mut node = pin!(task(ctx));
        // Узел OTA не опрошен: исхода `Start` нет, а `info` уже отвечен.
        poll(node.as_mut());
        for _ in 0..8 {
            poll(ota_node.as_mut());
            poll(node.as_mut());
        }
    }
    let replies: Vec<Response> = link.sent.iter().map(|f| decode(f)).collect();
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0].request_id, 2, "info — раньше исхода Start");
    assert_eq!(replies[1].request_id, 1);
    assert!(matches!(replies[1].result, Some(Response_::Result::Ok(_))));
}

/// Вторая команда обновления до исхода первой — `Busy` сразу, с её
/// номером; первая получает свой исход.
#[test]
fn a_second_update_command_in_flight_is_busy() {
    let finish = |id| request(id, Request_::Command::OtaFinish(proto::OtaFinish {}));
    let mut link = Link::with([Ok(finish(1)), Ok(finish(2))]);
    {
        let stand = supervisor::stand!(PROTOBUF_FRAG<Link, Service>);
        let mut ota_node =
            crate::ota::tests::serving(stand.ota_ask.server().expect("сервер ASK свободен"));
        let mut service = service(INFO);
        let response: &'static mut [u8] = vec![0; 64].leak();
        let mut ota_client = crate::ota::ClientId(0);
        let ctx = stand.ctx(&mut link, &mut service, response, &mut ota_client);
        let mut node = pin!(task(ctx));
        poll(node.as_mut());
        for _ in 0..8 {
            poll(ota_node.as_mut());
            poll(node.as_mut());
        }
    }
    let replies: Vec<Response> = link.sent.iter().map(|f| decode(f)).collect();
    assert_eq!(replies.len(), 2, "{replies:?}");
    assert_eq!(replies[0].request_id, 2);
    assert!(
        matches!(&replies[0].result, Some(Response_::Result::Error(e)) if e.text == "Busy"),
        "{replies:?}"
    );
    assert_eq!(replies[1].request_id, 1);
    assert!(
        matches!(&replies[1].result, Some(Response_::Result::Error(e)) if e.text == "NoSession"),
        "{replies:?}"
    );
}
// {%- endif %}
