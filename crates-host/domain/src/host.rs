//! Узел `HOST` графа задач: принимает сообщения protobuf от хоста по
//! кадровому каналу и отвечает на них.
//!
//! Единственное место, где сообщения схемы превращаются в типы приложения,
//! — обработчик [`HostService`](ports::HostService) приложения: в задачи
//! уходят проверенные значения своих типов. Узел только принимает кадр,
//! разбирает запрос, кодирует ответ и отправляет его. Схему узел не знает:
//! сообщения — параметры (`micropb::MessageDecode`/`MessageEncode`), пример
//! схемы — крейт `crates-host/proto`.
//!
//! Канал — [`FrameLink`](ports::FrameLink): кадр — одно сообщение целиком.
//! Датаграммы (UDP) дают его сами, поток (UART, USB CDC, TCP) — через
//! `adapters::cobs::Cobs`.
//!
//! Что узел делает с тем, что пришло:
//!
//! * кадр не разобрался как запрос — ответ
//!   [`HostService::malformed`](ports::HostService::malformed);
//! * кадр потерян или канал отказал — на приёме или на отправке — в лог, и
//!   узел ждёт следующий кадр: восстановить канал и подождать, пока он
//!   вернётся, — дело реализации `FrameLink` на следующем `recv`;
//! * ответ не помещается в буфер — в лог, хост получит таймаут; что буфер
//!   мал для какого-то ответа схемы, узел предупреждает уже при старте.
//!
//! Сообщения по инициативе устройства (события, телеметрия) — второй вход
//! узла; появятся по вопросу при генерации проекта.

// Обработчик по умолчанию над схемой `crates-host/proto` — только в проекте с
// протоколом protobuf: без него крейта схемы в проекте нет вовсе.
/// Обработчик по умолчанию над примером схемы `crates-host/proto`.
mod service;

pub use service::Service;

use defmt_or_log::{FormatOrDebug, info, warn};
use micropb::{DecodeError, MessageDecode, MessageEncode, PbEncoder};
use ports::{FrameLink, HostService, LinkError};
use supervisor::runtime::TaskExit;

// Вход узла — `HostWiring`: структуру эмитит сам фрагмент (`boot: …
// struct`), по полю на каждый слот, читающий `inputs.<поле>`. Строит её
// compose-site из полей платы и обработчика, подставляя их типы
// аргументами фрагмента:
//
// ```ignore
// fragments: [::domain::HOST_FRAG<HostLink, ::domain::host::Service> as HOST
//                 = ::domain::host::HostWiring { link: HostLink::new(board.serial, FRAME.init([0; 262])),
//                                                service: ::domain::host::Service::new("1.0.0"),
//                                                rx: RX.init([0; 258]), tx: TX.init([0; 256]) }];
// ```
//
// Буферы — `&'static mut`, а не массивы в узле: размер сообщения задаёт
// схема проекта, а не узел.

/// Узел `HOST`: отвечает хосту, пока жив канал.
///
/// Не выходит никогда: закрытый или отказавший канал — строка в лог, и
/// узел ждёт следующий кадр.
///
/// Без `watchdog:` намеренно — по правилу [`ota::run`](crate::ota::run):
/// узел законно ждёт запрос сколько угодно.
pub async fn run<L, S>(mut ctx: HostCtx<'_, L, S>) -> TaskExit
where
    L: FrameLink,
    L::Error: FormatOrDebug,
    S: HostService,
    S::Request: MessageDecode + Default,
    S::Response: MessageEncode,
{
    match ctx.run().await {}
}

#[fsm::typestate]
impl<L, S> HostCtx<'_, L, S>
where
    L: FrameLink,
    L::Error: FormatOrDebug,
    S: HostService,
    S::Request: MessageDecode + Default,
    S::Response: MessageEncode,
{
    fsm::machine! {
        machine Host mod edges {
            states {
                /// Ждёт кадр от хоста.
                Listening,
            }
            start => begin -> Listening;
            listen {
                Listening => frame: self.link.recv(self.rx),
            }
            transitions {
                Listening + frame(Ok(len))  => serve  -> self,
                Listening + frame(Err(ref err)) => failed -> self,
            }
        }
    }

    fn begin(&mut self) -> Listening {
        info!("domain: узел HOST запущен");
        if let Ok(max) = S::Response::MAX_SIZE
            && self.tx.len() < max
        {
            warn!(
                "host: буфер ответа {} байт, а самый длинный ответ схемы — {}: такие ответы не уйдут",
                self.tx.len(),
                max
            );
        }
        Listening
    }

    /// Разбирает запрос, отдаёт обработчику, отправляет ответ.
    async fn serve(&mut self, len: usize) {
        let mut request = S::Request::default();
        let response = match request.decode_from_bytes(&self.rx[..len]) {
            Ok(()) => self.service.handle(request).await,
            Err(err) => {
                warn!("host: кадр не разобрался: {}", decode_reason(&err));
                self.service.malformed()
            }
        };
        let Some(len) = encode(&response, self.tx) else {
            warn!(
                "host: ответ не помещается в буфер на {} байт",
                self.tx.len()
            );
            return;
        };
        if let Err(err) = self.link.send(&self.tx[..len]).await {
            warn!("host: ответ не ушёл: {:?}", &err);
        }
    }

    /// Кадр потерян или канал отказал — в лог; узел ждёт следующий кадр.
    #[expect(
        clippy::unused_self,
        reason = "сигнатуру действия задаёт таблица `#[fsm::typestate]`"
    )]
    fn failed(&mut self, err: &LinkError<L::Error>) {
        warn!("host: кадр не принят: {:?}", err);
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

// Узел объявлен рядом со своей задачей — как `OTA_FRAG` и `MODBUS_SLAVE_FRAG`;
// в граф шаблона попадёт по выбору протокола при генерации проекта.
//
// `<L, S>` — параметры фрагмента: канал платы и обработчик приложения
// `domain` не видит. `local` на обоих — канал и обработчик вправе быть
// `!Send` (периферия, ссылки на каналы задач). Буферы — обычные слоты:
// `&'static mut [u8]` — `Send`.
supervisor::supervisor_fragment! {
    name: HOST_FRAG<L, S>;
    boot: inputs: struct $crate::host::HostWiring;

    node deps: [], restart: Never,
        resources: [
            /// Кадровый канал к хосту.
            LINK: local L = inputs.link,
            /// Обработчик сообщений приложения.
            SERVICE: local S = inputs.service,
            /// Буфер приёма: самый длинный запрос и то, что канал кладёт туда
            /// сверх него (`adapters::cobs::rx_len` — ещё 2 байта CRC).
            RX: &'static mut [u8] = inputs.rx,
            /// Буфер закодированного ответа: не меньше самого длинного ответа
            /// (`MAX_SIZE` сообщения ответа, если схема его ограничивает).
            TX: &'static mut [u8] = inputs.tx
        ],
        ctx: $crate::host::HostCtx,
        task: $crate::host::run;
}

// Тесты гоняют узел с обработчиком над схемой `crates-host/proto`.
#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use core::pin::pin;
    use core::task::{Context, Waker};
    use micropb::{MessageDecode, MessageEncode, PbEncoder};
    use ports::{FrameLink, LinkError};
    use proto::{GetInfo, Ping, Request, Request_, Response, Response_, heapless};

    use super::{HostCtx, Service, run};

    /// Канал: отдаёт заранее заготовленные исходы `recv`, когда они кончаются,
    /// ждёт следующего кадра вечно — как настоящий канал. Отправленные кадры
    /// складывает в `sent`.
    struct Link {
        incoming: VecDeque<Result<Vec<u8>, LinkError<&'static str>>>,
        sent: Vec<Vec<u8>>,
        send: Result<(), LinkError<&'static str>>,
    }

    impl FrameLink for Link {
        type Error = &'static str;

        fn recv(
            &mut self,
            buf: &mut [u8],
        ) -> impl Future<Output = Result<usize, LinkError<Self::Error>>> {
            let next = self.incoming.pop_front().map(|next| {
                next.map(|frame| {
                    buf[..frame.len()].copy_from_slice(&frame);
                    frame.len()
                })
            });
            async move {
                match next {
                    Some(next) => next,
                    None => std::future::pending().await,
                }
            }
        }

        fn send(
            &mut self,
            frame: &[u8],
        ) -> impl Future<Output = Result<(), LinkError<Self::Error>>> {
            self.sent.push(frame.to_vec());
            std::future::ready(self.send)
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

    /// Гоняет узел по всем заготовленным кадрам, пока он не станет ждать
    /// следующий.
    fn serve(link: &mut Link, tx_len: usize) {
        let mut rx = vec![0; 64];
        let mut tx = vec![0; tx_len];
        let ctx = HostCtx {
            link,
            service: &mut Service::new("1.2.3"),
            rx: &mut rx,
            tx: &mut tx,
            index: 0,
        };
        let mut task = pin!(run(ctx));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(task.as_mut().poll(&mut cx).is_pending(), "ждёт кадр");
    }

    #[test]
    fn answers_requests_with_their_request_id() {
        let mut link = Link {
            incoming: VecDeque::from([
                Ok(request(7, Request_::Command::Ping(Ping::default()))),
                Ok(request(8, Request_::Command::GetInfo(GetInfo::default()))),
            ]),
            sent: Vec::new(),
            send: Ok(()),
        };
        serve(&mut link, 64);
        let replies: Vec<Response> = link.sent.iter().map(|f| decode(f)).collect();
        assert_eq!(replies[0].request_id, 7);
        assert!(matches!(
            replies[0].result,
            Some(Response_::Result::Pong(_))
        ));
        assert_eq!(replies[1].request_id, 8);
        assert!(
            matches!(&replies[1].result, Some(Response_::Result::Info(i)) if i.version == "1.2.3")
        );
    }

    #[test]
    fn malformed_frame_gets_an_error_and_lost_frame_is_skipped() {
        let mut link = Link {
            incoming: VecDeque::from([
                Ok(vec![0xFF, 0xFF, 0xFF]),
                Err(LinkError::Lost("помеха")),
                Ok(request(9, Request_::Command::Ping(Ping::default()))),
            ]),
            sent: Vec::new(),
            send: Ok(()),
        };
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

    /// Канал, отказавший на отправке, узел не останавливает: он ждёт
    /// следующий кадр и отвечает на него.
    #[test]
    fn failed_send_does_not_stop_the_node() {
        let mut link = Link {
            incoming: VecDeque::from([
                Ok(request(1, Request_::Command::Ping(Ping::default()))),
                Ok(request(2, Request_::Command::Ping(Ping::default()))),
            ]),
            sent: Vec::new(),
            send: Err(LinkError::Down("обрыв")),
        };
        serve(&mut link, 64);
        assert_eq!(link.sent.len(), 2, "оба запроса обработаны");
    }

    /// Ответ, не принятый каналом (не влез в кадр транспорта), — потеря
    /// кадра, а не отказ канала: узел читает дальше.
    #[test]
    fn lost_send_keeps_the_node_listening() {
        let mut link = Link {
            incoming: VecDeque::from([
                Ok(request(1, Request_::Command::Ping(Ping::default()))),
                Ok(request(2, Request_::Command::Ping(Ping::default()))),
            ]),
            sent: Vec::new(),
            send: Err(LinkError::Lost("не влез")),
        };
        serve(&mut link, 64);
        assert_eq!(link.sent.len(), 2, "оба запроса обработаны");
    }

    #[test]
    fn reply_that_does_not_fit_is_dropped_not_truncated() {
        let mut link = Link {
            incoming: VecDeque::from([
                Ok(request(1, Request_::Command::GetInfo(GetInfo::default()))),
                Ok(request(2, Request_::Command::Ping(Ping::default()))),
            ]),
            sent: Vec::new(),
            send: Ok(()),
        };
        // `pong` с id влезает в 4 байта, `info` с версией — нет.
        serve(&mut link, 4);
        assert_eq!(link.sent.len(), 1);
        assert_eq!(decode(&link.sent[0]).request_id, 2);
    }

    /// Запрос без команды — тоже запрос: ответ-ошибка с тем же `request_id`.
    #[test]
    fn a_request_without_a_command_gets_an_error_reply() {
        let mut link = Link {
            incoming: VecDeque::from([Ok(encode(&Request {
                request_id: 4,
                command: None,
            }))]),
            sent: Vec::new(),
            send: Ok(()),
        };
        serve(&mut link, 64);
        assert_eq!(link.sent.len(), 1);
        let reply = decode(&link.sent[0]);
        assert_eq!(reply.request_id, 4);
        assert!(matches!(reply.result, Some(Response_::Result::Error(_))));
    }

    /// Закрытый канал узел не останавливает: следующий кадр — ответ.
    #[test]
    fn a_closed_link_does_not_stop_the_node() {
        let mut link = Link {
            incoming: VecDeque::from([
                Err(LinkError::Down("кабель вынут")),
                Ok(request(5, Request_::Command::Ping(Ping::default()))),
            ]),
            sent: Vec::new(),
            send: Ok(()),
        };
        serve(&mut link, 64);
        assert_eq!(link.sent.len(), 1, "ответ после отказа");
        assert_eq!(decode(&link.sent[0]).request_id, 5);
    }
}
