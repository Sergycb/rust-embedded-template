//! Узел `PROTOBUF`: сообщения protobuf от хоста над кадровым каналом.
// {%- if graph == "true" %}
//! В графе — узел `PROTOBUF_<транспорт>`.
// {%- endif %}
//!
//! Единственное место, где сообщения схемы превращаются в типы приложения,
//! — обработчик [`Service`](crate::protobuf::Service): в задачи уходят проверенные значения своих
//! типов. Узел только разбирает запрос из кадра и кодирует ответ; схема —
//! крейт `crates-host/proto`, проект правит её и [`Service`](crate::protobuf::Service) здесь.
//!
//! Канал под ним — [`FrameDuplex`](ports::FrameDuplex), кадр — одно
//! сообщение целиком.
// {%- if has_udp == "true" %}
//! Датаграммы дают его сами.
// {%- endif %}
// {%- if cobs_used == "true" %}
//! Поток байтов — через `adapters::cobs::Cobs`.
// {%- endif %}
//!
//! Что узел делает с кадром:
//!
//! * кадр не разобрался как запрос — ответ [`Service::malformed`](crate::protobuf::Service::malformed);
//! * остальное — [`Service::handle`](crate::protobuf::Service::handle) и ответ{% if ota == "true" %} (команда обновления — узлу OTA на вход
//!   `CMD`, и ответ ждёт её исхода: пока OTA стирает раздел, канал не
//!   обслуживается, как у Modbus){% endif %};
//! * ответ не помещается в буфер — в лог и без ответа, хост получит таймаут.
//!
//! Ответ кодируется в свой буфер узла (`RESPONSE_MAX` байт) и уходит
//! `send_from`.
//!
//! Сообщения по инициативе устройства (события, телеметрия) — второй вход
//! узла; появятся по вопросу при генерации проекта.

mod codec;
// Обработчик по умолчанию над схемой `crates-host/proto` — только в проекте с
// протоколом protobuf: без него крейта схемы в проекте нет вовсе.
/// Обработчик по умолчанию над примером схемы `crates-host/proto`.
mod service;

use defmt_or_log::{FormatOrDebug, info, warn};
use micropb::MessageDecode;
use ports::{FrameDuplex, LinkError};
use proto::Request;

use crate::protobuf::codec::{decode_reason, encode};

pub use service::{DeviceInfo, REQUEST_MAX, RESPONSE_MAX, Service};

// Узел объявлен рядом со своей задачей, по узлу на транспорт.
//
// `<L>` — параметр фрагмента: канал платы `domain` не видит. `local` на
// канале и обработчике — канал вправе быть `!Send` (периферия, сокет).
// Буфер ответа — память, которую объявил граф (`static [u8; RESPONSE_MAX]`).
supervisor::supervisor_fragment! {
    name: PROTOBUF_FRAG<L>;
    boot: inputs: struct $crate::protobuf::ProtobufWiring;

    node resources: [
            /// Кадровый канал.
            LINK: local L = inputs.link,
            /// Обработчик сообщений.
            SERVICE: local $crate::protobuf::Service = inputs.service,
            /// Буфер ответа.
            RESPONSE: &'static mut [u8] = inputs.response
        ],
        ctx: $crate::protobuf::ProtobufCtx,
        task: $crate::protobuf::task;
}

#[fsm::typestate]
impl<L> ProtobufCtx<'_, L>
where
    L: FrameDuplex,
    L::Error: FormatOrDebug,
{
    fsm::machine! {
        machine Protobuf mod edges {
            states {
                /// Ждёт кадр.
                Listening,
            }
            start => begin -> Listening;
            listen {
                Listening => frame: self.link.recv(),
            }
            transitions {
                Listening + frame(Ok(len))      => serve  -> self,
                Listening + frame(Err(ref err)) => failed -> self,
            }
        }
    }

    fn begin(&mut self) -> Listening {
        info!(
            "domain: узел PROTOBUF запущен, буфер ответа {} байт",
            self.response.len()
        );
        Listening
    }

    /// Разбирает кадр и отвечает на запрос.
    async fn serve(&mut self, len: usize) {
        let mut request = Request::default();
        let response = match request.decode_from_bytes(&self.link.buffer()[..len]) {
            Ok(()) => self.service.handle(request).await,
            Err(err) => {
                warn!("protobuf: кадр не разобрался: {}", decode_reason(&err));
                self.service.malformed()
            }
        };
        self.respond(&response).await;
    }
    /// Кадр потерян или канал отказал — в лог; узел ждёт следующий кадр.
    #[expect(
        clippy::unused_self,
        reason = "сигнатуру действия задаёт таблица `#[fsm::typestate]`"
    )]
    fn failed(&mut self, err: &LinkError<L::Error>) {
        warn!("protobuf: кадр не принят: {:?}", err);
    }

    /// Ответ — в буфер ответа и в канал; не поместился или не ушёл — в лог.
    async fn respond(&mut self, response: &proto::Response) {
        let Some(len) = encode(response, self.response) else {
            warn!(
                "protobuf: ответ не помещается в буфер на {} байт",
                self.response.len()
            );
            return;
        };
        if let Err(err) = self.link.send_from(&self.response[..len]).await {
            warn!("protobuf: ответ не ушёл: {:?}", &err);
        }
    }
}

/// Узел `PROTOBUF`: обслуживает канал, пока жив, и не выходит никогда.
///
/// Без `watchdog:` намеренно: узел законно
/// ждёт запрос сколько угодно, и отметка по таймеру сторожила бы
/// исполнитель, а не работу.
pub async fn task<L>(mut ctx: ProtobufCtx<'_, L>)
where
    L: FrameDuplex,
    L::Error: FormatOrDebug,
{
    match ctx.run().await {}
}

#[cfg(test)]
mod tests {
    //! Тесты узла `PROTOBUF`: гоняют узел над схемой `crates-host/proto`.

    use core::future::poll_fn;
    use core::pin::{Pin, pin};
    use core::task::{Context, Poll, Waker};
    use std::collections::VecDeque;

    use micropb::{MessageDecode, MessageEncode, PbEncoder};
    use ports::{FrameDuplex, FrameLink, LinkError};
    use proto::{Get, Request, Request_, Response, Response_, heapless};

    use super::service::tests::INFO;
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
        fn with(
            incoming: impl IntoIterator<Item = Result<Vec<u8>, LinkError<&'static str>>>,
        ) -> Self {
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

    fn poll(node: Pin<&mut (impl Future<Output = ()> + ?Sized)>) {
        let mut cx = Context::from_waker(Waker::noop());
        assert!(node.poll(&mut cx).is_pending(), "узел не выходит");
    }

    /// Гоняет узел над `link` с буфером ответа на `capacity` байт по всем
    /// заготовленным кадрам, пока он не станет ждать следующий{% if ota == "true" %}; узел OTA за
    /// входом `CMD` опрашивается по очереди с ним, так что и команда обновления
    /// получает ответ{% endif %}.
    fn serve(link: &mut Link, capacity: usize) {
        let stand = supervisor::stand!(PROTOBUF_FRAG<Link>);
        // {%- if ota == "true" %}
        let (ota, mut ota_node) = crate::ota::tests::live();
        // {%- endif %}
        let mut service = Service::new(
            INFO, // {%- if ota == "true" %}
            ota,
            // {%- endif %}
        );
        let response: &'static mut [u8] = vec![0; capacity].leak();
        let mut node = pin!(task(stand.ctx(link, &mut service, response)));
        poll(node.as_mut());
        // {%- if ota == "true" %}
        for _ in 0..8 {
            poll(ota_node.as_mut());
            poll(node.as_mut());
        }
        // {%- endif %}
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
        assert!(
            matches!(&replies[1].result, Some(Response_::Result::Info(i)) if i.version == "1.2.3")
        );
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
        /// схема, команда обновления, — узел не падает, отвечает на него ровно
        /// одним кадром, и этот кадр — ответ схемы.
        #[test]
        fn any_frame_gets_at_most_one_well_formed_reply(
            frame in proptest::collection::vec(proptest::num::u8::ANY, 0..128)
        ) {
            let mut link = Link::with([Ok(frame)]);
            serve(&mut link, 256);
            proptest::prop_assert_eq!(link.sent.len(), 1, "{} ответов", link.sent.len());
            let mut response = Response::default();
            proptest::prop_assert!(response.decode_from_bytes(&link.sent[0]).is_ok());
        }
    }
}
