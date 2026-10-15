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
//! * кадр потерян на приёме или на отправке
//!   ([`LinkError::Lost`](ports::LinkError::Lost)) — в лог, ждёт следующий;
//!   [`MAX_LOST_IN_A_ROW`](crate::host::MAX_LOST_IN_A_ROW) потерь подряд —
//!   как отказ канала;
//! * канал закрыт или отказал ([`LinkError::Down`](ports::LinkError::Down))
//!   — выход `Failed`, граф перезапустит узел с backoff'ом, а канал на
//!   следующем `recv` восстановит реализация `FrameLink`;
//! * ответ не помещается в буфер — в лог, хост получит таймаут; что буфер
//!   мал для какого-то ответа схемы, узел предупреждает уже при старте.
//!
//! Сообщения по инициативе устройства (события, телеметрия) — второй вход
//! узла; появятся по вопросу при генерации проекта.

/// Обработчик по умолчанию над примером схемы `crates-host/proto`.
mod service;

pub use service::Service;

use core::fmt::Debug;

use defmt_or_log::{Debug2Format, info, warn};
use embassy_time::Duration;
use micropb::{MessageDecode, MessageEncode, PbEncoder};
use ports::{FrameLink, HostService, LinkError};
use supervisor::policy::{BackoffPolicy, JitterPolicy};
use supervisor::runtime::TaskExit;

/// Политика пауз между перезапусками узла — те же цифры, что у
/// [`ota::BACKOFF`](crate::ota::BACKOFF), и своя константа намеренно:
/// фрагмент самодостаточен. Пауза нужна и на случай канала, который
/// отказывает сразу: перезапуск без неё крутился бы впустую.
pub const BACKOFF: BackoffPolicy = BackoffPolicy {
    first: Duration::from_millis(50),
    factor: 2,
    jitter: JitterPolicy::None,
    floor: Duration::from_millis(50),
    max: Duration::from_secs(5),
};

/// Сколько потерянных кадров подряд узел терпит, прежде чем считать канал
/// отказавшим.
///
/// Шум на линии теряет кадры по одному, и между потерями приходят байты —
/// приём ждёт. Поток, который на каждый `read` сразу отвечает ошибкой
/// (сломанная периферия: `embassy-stm32` отдаёт все ошибки UART одним
/// `ErrorKind::Other`), не ждёт никогда, и без предела узел крутил бы
/// цикл, не отдавая исполнитель другим задачам. Перезапуск с backoff'ом
/// вставляет паузу. Значение — с запасом на пачку помех, сбросом счётчика
/// служит любой принятый кадр.
pub const MAX_LOST_IN_A_ROW: u8 = 32;

/// Что узлу нужно на входе. Строит compose-site из полей платы и
/// обработчика приложения, подставляя их типы аргументами фрагмента:
///
/// ```ignore
/// fragments: [::domain::HOST_FRAG<HostLink, ::domain::host::Service> as HOST
///                 = ::domain::host::HostWiring { link: HostLink::new(board.serial, FRAME.init([0; 262])),
///                                                service: ::domain::host::Service::new("1.0.0"),
///                                                rx: RX.init([0; 258]), tx: TX.init([0; 256]) }];
/// ```
///
/// Буферы — `&'static mut`, а не массивы в узле: размер сообщения задаёт
/// схема проекта, а не узел.
#[derive(Debug)]
pub struct HostWiring<L, S> {
    /// Кадровый канал к хосту.
    pub link: L,
    /// Обработчик сообщений приложения.
    pub service: S,
    /// Буфер приёма: самый длинный запрос и то, что канал кладёт туда
    /// сверх него (`adapters::cobs::Cobs::rx_len` — ещё 2 байта CRC).
    pub rx: &'static mut [u8],
    /// Буфер закодированного ответа: не меньше самого длинного ответа
    /// (`MAX_SIZE` сообщения ответа, если схема его ограничивает).
    pub tx: &'static mut [u8],
}

/// Узел `HOST`: отвечает хосту, пока жив канал.
///
/// `TaskExit::Failed` — только когда канал закрыт или отказал;
/// `Completed` не возвращается никогда.
///
/// Без `watchdog:` намеренно — по правилу [`ota::run`](crate::ota::run):
/// узел законно ждёт запрос сколько угодно.
pub async fn run<L, S>(mut ctx: HostCtx<'_, L, S>) -> TaskExit
where
    L: FrameLink,
    L::Error: Debug,
    S: HostService,
    S::Request: MessageDecode + Default,
    S::Response: MessageEncode,
{
    ctx.run().await.result
}

#[fsm::typestate]
impl<L, S> HostCtx<'_, L, S>
where
    L: FrameLink,
    L::Error: Debug,
    S: HostService,
    S::Request: MessageDecode + Default,
    S::Response: MessageEncode,
{
    fsm::machine! {
        machine Host mod edges {
            states {
                /// Ждёт кадр от хоста; `lost` — сколько кадров подряд
                /// потеряно.
                Listening { lost: u8 },
                /// Канал закрыт или отказал: прогон окончен.
                exit Down { result: TaskExit },
            }
            start => begin -> Listening;
            listen {
                Listening { .. } => frame: self.link.recv(self.rx),
            }
            transitions {
                Listening + frame(Ok(len))                  => serve -> Listening | Down,
                Listening + frame(Err(LinkError::Lost(err))) => lost  -> Listening | Down,
                Listening + frame(Err(LinkError::Down(err))) => down  -> Down,
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
        Listening { lost: 0 }
    }

    /// Разбирает запрос, отдаёт обработчику, отправляет ответ.
    async fn serve(&mut self, s: Listening, len: usize) -> edges::Serve {
        let mut request = S::Request::default();
        let response = match request.decode_from_bytes(&self.rx[..len]) {
            Ok(()) => self.service.handle(request).await,
            Err(err) => {
                warn!("host: кадр не разобрался: {:?}", Debug2Format(&err));
                self.service.malformed()
            }
        };
        let Some(len) = encode(&response, self.tx) else {
            warn!(
                "host: ответ не помещается в буфер на {} байт",
                self.tx.len()
            );
            return Listening { lost: 0 }.into();
        };
        match self.link.send(&self.tx[..len]).await {
            Ok(()) => Listening { lost: 0 }.into(),
            Err(LinkError::Lost(err)) => match self.lost(&s, err) {
                edges::Lost::Listening(next) => next.into(),
                edges::Lost::Down(next) => next.into(),
            },
            Err(LinkError::Down(err)) => self.down(s, err).into(),
        }
    }

    /// Кадр потерян — канал жив, ждём следующий; слишком много подряд —
    /// канал считается отказавшим.
    #[expect(
        clippy::unused_self,
        reason = "сигнатуру действия задаёт таблица `#[fsm::typestate]`"
    )]
    fn lost(&mut self, s: &Listening, err: L::Error) -> edges::Lost {
        warn!("host: кадр потерян: {:?}", Debug2Format(&err));
        let lost = s.lost.saturating_add(1);
        if lost < MAX_LOST_IN_A_ROW {
            Listening { lost }.into()
        } else {
            warn!("host: {} кадров потеряно подряд — канал не работает", lost);
            Down {
                result: TaskExit::Failed,
            }
            .into()
        }
    }

    /// Канал закрыт или отказал.
    #[expect(
        clippy::unused_self,
        reason = "сигнатуру действия задаёт таблица `#[fsm::typestate]`"
    )]
    fn down(&mut self, _: Listening, err: L::Error) -> Down {
        warn!("host: канал закрыт: {:?}", Debug2Format(&err));
        Down {
            result: TaskExit::Failed,
        }
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
    boot: inputs: $crate::host::HostWiring<L, S>;

    node deps: [], restart: ::supervisor::policy::RestartPolicy::OnFailure,
        backoff: $crate::host::BACKOFF,
        resources: [
            LINK: local L = inputs.link,
            SERVICE: local S = inputs.service,
            RX: &'static mut [u8] = inputs.rx,
            TX: &'static mut [u8] = inputs.tx
        ],
        ctx: $crate::host::HostCtx,
        task: $crate::host::run;
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;

    use micropb::{MessageDecode, MessageEncode, PbEncoder};
    use ports::{FrameLink, LinkError};
    use proto::{GetInfo, Ping, Request, Request_, Response, Response_, heapless};
    use supervisor::runtime::TaskExit;

    use super::{HostCtx, Service, run};

    /// Канал: отдаёт заранее заготовленные исходы `recv`, когда они кончаются,
    /// — закрыт. Отправленные кадры складывает в `sent`.
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
            let next = self
                .incoming
                .pop_front()
                .unwrap_or(Err(LinkError::Down("закрыт")))
                .map(|frame| {
                    buf[..frame.len()].copy_from_slice(&frame);
                    frame.len()
                });
            std::future::ready(next)
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

    /// Гоняет узел, пока канал не закроется; возвращает исход и отправленное.
    async fn serve(link: &mut Link, tx_len: usize) -> TaskExit {
        let mut rx = vec![0; 64];
        let mut tx = vec![0; tx_len];
        let ctx = HostCtx {
            link,
            service: &mut Service::new("1.2.3"),
            rx: &mut rx,
            tx: &mut tx,
            index: 0,
        };
        run(ctx).await
    }

    #[tokio::test]
    async fn answers_requests_with_their_request_id() {
        let mut link = Link {
            incoming: VecDeque::from([
                Ok(request(7, Request_::Command::Ping(Ping::default()))),
                Ok(request(8, Request_::Command::GetInfo(GetInfo::default()))),
            ]),
            sent: Vec::new(),
            send: Ok(()),
        };
        assert_eq!(serve(&mut link, 64).await, TaskExit::Failed);
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

    #[tokio::test]
    async fn malformed_frame_gets_an_error_and_lost_frame_is_skipped() {
        let mut link = Link {
            incoming: VecDeque::from([
                Ok(vec![0xFF, 0xFF, 0xFF]),
                Err(LinkError::Lost("помеха")),
                Ok(request(9, Request_::Command::Ping(Ping::default()))),
            ]),
            sent: Vec::new(),
            send: Ok(()),
        };
        assert_eq!(serve(&mut link, 64).await, TaskExit::Failed);
        assert_eq!(link.sent.len(), 2, "на потерянный кадр ответа нет");
        let malformed = decode(&link.sent[0]);
        assert_eq!(malformed.request_id, 0);
        assert!(matches!(
            malformed.result,
            Some(Response_::Result::Error(_))
        ));
        assert_eq!(decode(&link.sent[1]).request_id, 9);
    }

    #[tokio::test]
    async fn failed_send_ends_the_run() {
        let mut link = Link {
            incoming: VecDeque::from([
                Ok(request(1, Request_::Command::Ping(Ping::default()))),
                Ok(request(2, Request_::Command::Ping(Ping::default()))),
            ]),
            sent: Vec::new(),
            send: Err(LinkError::Down("обрыв")),
        };
        assert_eq!(serve(&mut link, 64).await, TaskExit::Failed);
        assert_eq!(
            link.sent.len(),
            1,
            "после отказа отправки узел не читает дальше"
        );
    }

    /// Ответ, не принятый каналом (не влез в кадр транспорта), — потеря
    /// кадра, а не отказ канала: узел читает дальше.
    #[tokio::test]
    async fn lost_send_does_not_end_the_run() {
        let mut link = Link {
            incoming: VecDeque::from([
                Ok(request(1, Request_::Command::Ping(Ping::default()))),
                Ok(request(2, Request_::Command::Ping(Ping::default()))),
            ]),
            sent: Vec::new(),
            send: Err(LinkError::Lost("не влез")),
        };
        assert_eq!(serve(&mut link, 64).await, TaskExit::Failed);
        assert_eq!(link.sent.len(), 2, "оба запроса обработаны");
    }

    /// Поток, который отказывает на каждом чтении, не крутит узел вечно:
    /// после `MAX_LOST_IN_A_ROW` потерь подряд — выход, и паузу вставит
    /// backoff графа. Принятый кадр счётчик сбрасывает.
    #[tokio::test]
    async fn too_many_lost_frames_in_a_row_end_the_run() {
        let lost = usize::from(super::MAX_LOST_IN_A_ROW);
        let mut incoming: VecDeque<_> =
            (0..lost - 1).map(|_| Err(LinkError::Lost("шум"))).collect();
        incoming.push_back(Ok(request(1, Request_::Command::Ping(Ping::default()))));
        incoming.extend((0..lost).map(|_| Err(LinkError::Lost("шум"))));
        incoming.push_back(Ok(request(2, Request_::Command::Ping(Ping::default()))));
        let mut link = Link {
            incoming,
            sent: Vec::new(),
            send: Ok(()),
        };
        assert_eq!(serve(&mut link, 64).await, TaskExit::Failed);
        assert_eq!(link.sent.len(), 1, "после серии потерь узел вышел");
        assert_eq!(link.incoming.len(), 1, "последний запрос не прочитан");
    }

    #[tokio::test]
    async fn reply_that_does_not_fit_is_dropped_not_truncated() {
        let mut link = Link {
            incoming: VecDeque::from([
                Ok(request(1, Request_::Command::GetInfo(GetInfo::default()))),
                Ok(request(2, Request_::Command::Ping(Ping::default()))),
            ]),
            sent: Vec::new(),
            send: Ok(()),
        };
        // `pong` с id влезает в 4 байта, `info` с версией — нет.
        assert_eq!(serve(&mut link, 4).await, TaskExit::Failed);
        assert_eq!(link.sent.len(), 1);
        assert_eq!(decode(&link.sent[0]).request_id, 2);
    }
}
