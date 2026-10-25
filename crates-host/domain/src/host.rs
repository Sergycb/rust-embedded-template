//! Узел `HOST` графа задач: сообщения protobuf от хоста над кадровым каналом
//! (в графе — узел `HOST_<транспорт>`).
//!
//! Единственное место, где сообщения схемы превращаются в типы приложения,
//! — обработчик [`HostService`](ports::HostService) приложения: в задачи
//! уходят проверенные значения своих типов. Узел только разбирает запрос из
//! кадра и кодирует ответ. Схему он не знает: сообщения — параметры
//! (`micropb::MessageDecode`/`MessageEncode`), пример схемы — крейт
//! `crates-host/proto`.
//!
//! Канал под ним — [`FrameDuplex`](ports::FrameDuplex), кадр — одно
//! сообщение целиком: датаграммы (UDP) дают его сами, поток (UART, USB CDC,
//! TCP) — через `adapters::cobs::Cobs`.
//!
//! Что узел делает с кадром:
//!
//! * кадр не разобрался как запрос — ответ
//!   [`HostService::malformed`](ports::HostService::malformed);
//! * {% if ota == "true" %}команда обновления (`HostService::route`) — узлу OTA на вход `ASK`, не
//!   дожидаясь исхода: канал обслуживается дальше, а исход, когда придёт,
//!   уходит хосту ответом с тем же `request_id` — ответы на другие запросы
//!   могут его обогнать;
//! * {% endif %}остальное — [`HostService::handle`](ports::HostService::handle) и ответ;
//! * ответ не помещается в буфер — в лог и без ответа, хост получит таймаут;
//!   что буфер мал для какого-то ответа схемы, узел предупреждает уже при
//!   старте.
//!
//! Ответ кодируется в свой буфер узла и уходит `send_from`: в буфере канала
//! может лежать недочитанный кадр — его приём бросила гонка с исходом.
//!
//! Сообщения по инициативе устройства (события, телеметрия) — второй вход
//! узла; появятся по вопросу при генерации проекта.

// Обработчик по умолчанию над схемой `crates-host/proto` — только в проекте с
// протоколом protobuf: без него крейта схемы в проекте нет вовсе.
/// Обработчик по умолчанию над примером схемы `crates-host/proto`.
mod service;

pub use service::{DeviceInfo, Service};

use defmt_or_log::{FormatOrDebug, info, warn};
use micropb::{DecodeError, MessageDecode, MessageEncode, PbEncoder};
use ports::{Assemble, FrameDuplex, HostService, LinkError};
// {%- if ota == "true" %}
use ports::Route;
// {%- endif %}
use supervisor::runtime::TaskExit;

/// Вход узла `HOST`: из чего собрать канал `L`, куда кодировать ответ и чем
/// отвечать.
///
/// Канал собирает узел (`L::assemble(source, buffers)` — порт
/// [`Assemble`]), а compose-site только раздаёт:
///
/// ```ignore
/// fragments: [::domain::HOST_FRAG<crate::link::uart::usart1::Link, ::domain::host::Service>
///                 as HOST_USART1 = ::domain::host::HostWiring {
///                     source: board.usart1,
///                     buffers: crate::link::uart::usart1::buffers(),
///                     response: crate::link::uart::usart1::response(),
///                     service: ::domain::host::Service::new(crate::device_info(board.spec)),
///                     ota_client: ::domain::ota::ClientId(0),   // только с OTA
///                 }];
/// ```
///
/// Написан руками, а не выведен фрагментом: слот канала инициализируется
/// сборкой из двух полей, а не чтением одного.
#[derive(Debug)]
pub struct HostWiring<L: Assemble, S> {
    /// Рычаг платы, над которым собирается канал.
    pub source: L::Source,
    /// Память канала от приложения.
    pub buffers: L::Buffers,
    /// Буфер ответа — не короче самого длинного ответа схемы.
    pub response: &'static mut [u8],
    /// Обработчик сообщений приложения.
    pub service: S,
    // {%- if ota == "true" %}
    /// Номер клиента узла OTA — транспорта: с ним команды обновления уходят
    /// на вход `ASK` (`asks:` фрагмента).
    pub ota_client: crate::ota::ClientId,
    // {%- endif %}
}

#[fsm::typestate]
impl<L, S> HostCtx<'_, L, S>
where
    L: FrameDuplex,
    L::Error: FormatOrDebug,
    S: HostService,
    S::Request: MessageDecode + Default,
    S::Response: MessageEncode,
{
    fsm::machine! {
        machine Host mod edges {
            states {
                /// Ждёт кадр.
                Listening,
            }
            start => begin -> Listening;
            listen {
                // {%- if ota == "true" %}
                // Исход — первым: держит квоту, пока не забран, и уходит
                // хосту сразу, а не за всеми кадрами в очереди.
                Listening => ota: self.ota_ask.reply(),
                // {%- endif %}
                Listening => frame: self.link.recv(),
            }
            transitions {
                Listening + frame(Ok(len))      => serve    -> self,
                Listening + frame(Err(ref err)) => failed   -> self,
                // {%- if ota == "true" %}
                Listening + ota(reply)          => answered -> self,
                // {%- endif %}
            }
        }
    }

    fn begin(&mut self) -> Listening {
        let capacity = self.response.len();
        info!("domain: узел HOST запущен, буфер ответа {} байт", capacity);
        if let Ok(max) = S::Response::MAX_SIZE
            && capacity < max
        {
            warn!(
                "host: буфер ответа {} байт, а самый длинный ответ схемы — {}: такие ответы не уйдут",
                capacity, max
            );
        }
        Listening
    }

    /// Разбирает кадр и отвечает на запрос{% if ota == "true" %}; команду обновления отдаёт
    /// узлу OTA, не дожидаясь исхода{% endif %}.
    async fn serve(&mut self, len: usize) {
        let mut request = S::Request::default();
        let response = match request.decode_from_bytes(&self.link.buffer()[..len]) {
            Ok(()) => {
                // {%- if ota == "true" %}
                let request = match self.service.route(request) {
                    Route::Handle(request) => request,
                    Route::Reply(response) => return self.respond(&response).await,
                    Route::Ota {
                        request_id,
                        command,
                    } => return self.ota(request_id, command).await,
                };
                // {%- endif %}
                self.service.handle(request).await
            }
            Err(err) => {
                warn!("host: кадр не разобрался: {}", decode_reason(&err));
                self.service.malformed()
            }
        };
        self.respond(&response).await;
    }
    // {%- if ota == "true" %}

    /// Команда обновления — на вход `ASK` узла OTA; исход принесёт `reply()`
    /// (`answered`). Прежняя команда ещё без исхода (квота — одна) — ответ
    /// `Busy` сразу.
    async fn ota(&mut self, request_id: u32, command: ports::OtaCommand) {
        let request = crate::ota::OtaRequest {
            client: *self.ota_client,
            command,
        };
        if let Err(busy) = self.ota_ask.try_call(request_id, request) {
            warn!(
                "host: команда обновления {} отвергнута: прежняя ещё выполняется",
                request_id
            );
            let response = self
                .service
                .ota_outcome(busy.tag, Err(ports::Rejection::Busy));
            self.respond(&response).await;
        }
    }

    /// Исход команды обновления — хосту ответом на её запрос.
    async fn answered(&mut self, reply: supervisor::AskReply<u32, crate::ota::OtaReply>) {
        let outcome = reply.outcome.unwrap_or_else(|_dropped| {
            warn!("host: узел OTA не ответил");
            Err(ports::Rejection::Device)
        });
        let response = self.service.ota_outcome(reply.tag, outcome);
        self.respond(&response).await;
    }
    // {%- endif %}

    /// Кадр потерян или канал отказал — в лог; узел ждёт следующий кадр.
    #[expect(
        clippy::unused_self,
        reason = "сигнатуру действия задаёт таблица `#[fsm::typestate]`"
    )]
    fn failed(&mut self, err: &LinkError<L::Error>) {
        warn!("host: кадр не принят: {:?}", err);
    }

    /// Ответ — в буфер ответа и в канал; не поместился или не ушёл — в лог.
    async fn respond(&mut self, response: &S::Response) {
        let Some(len) = encode(response, self.response) else {
            warn!(
                "host: ответ не помещается в буфер на {} байт",
                self.response.len()
            );
            return;
        };
        if let Err(err) = self.link.send_from(&self.response[..len]).await {
            warn!("host: ответ не ушёл: {:?}", &err);
        }
    }
}

/// Узел `HOST`: обслуживает канал, пока жив, и не выходит никогда.
///
/// Без `watchdog:` намеренно — по правилу `heartbeat::task`: узел законно
/// ждёт запрос сколько угодно, и отметка по таймеру сторожила бы
/// исполнитель, а не работу.
pub async fn task<L, S>(mut ctx: HostCtx<'_, L, S>) -> TaskExit
where
    L: FrameDuplex,
    L::Error: FormatOrDebug,
    S: HostService,
    S::Request: MessageDecode + Default,
    S::Response: MessageEncode,
{
    match ctx.run().await {}
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

// Узел объявлен рядом со своей задачей — как `SERVER_FRAG`; в граф шаблона
// попадёт по узлу на транспорт с протоколом protobuf.
//
// `<L, S>` — параметры фрагмента: канал платы и обработчик приложения
// `domain` в графе не видит. `local` на обоих — канал вправе быть `!Send`
// (периферия, сокет), обработчик — тоже. Буфер ответа — ссылка на статик
// приложения (`crate::link::<вид>`): его размер — решение приложения.
supervisor::supervisor_fragment! {
    name: HOST_FRAG<L, S>;
    boot: inputs: $crate::host::HostWiring<L, S>;

    node deps: [], restart: Never,
        resources: [
            /// Кадровый канал — собирает его узел из рычага платы и памяти
            /// приложения.
            LINK: local L = <L as $crate::Assemble>::assemble(inputs.source, inputs.buffers),
            /// Обработчик сообщений приложения.
            SERVICE: local S = inputs.service,
            /// Буфер ответа.
            RESPONSE: &'static mut [u8] = inputs.response,
            // {%- if ota == "true" %}
            /// Номер клиента узла OTA — транспорта.
            OTA_CLIENT: $crate::ota::ClientId = inputs.ota_client
            // {%- endif %}
        ],
        // {%- if ota == "true" %}
        // Команды обновления — на вход `ASK` узла OTA без ожидания исхода:
        // узел обслуживает канал, пока OTA стирает раздел. Метка — номер
        // запроса (`request_id`), квота — одна команда в полёте.
        asks: [OTA.ASK: $crate::ota::OtaRequest -> $crate::ota::OtaReply; 1 tag u32],
        // {%- endif %}
        ctx: $crate::host::HostCtx,
        task: $crate::host::task;
}

// Тесты гоняют узел над схемой `crates-host/proto`.
#[cfg(test)]
mod tests {
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

    fn poll(node: Pin<&mut (impl Future<Output = TaskExit> + ?Sized)>) {
        let mut cx = Context::from_waker(Waker::noop());
        assert!(node.poll(&mut cx).is_pending(), "узел не выходит");
    }

    /// Гоняет узел над `link` с буфером ответа на `capacity` байт по всем
    /// заготовленным кадрам, пока он не станет ждать следующий.
    fn serve(link: &mut Link, capacity: usize) {
        let stand = supervisor::stand!(HOST_FRAG<Link, Service>);
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
            let stand = supervisor::stand!(HOST_FRAG<Link, Service>);
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
            let stand = supervisor::stand!(HOST_FRAG<Link, Service>);
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
}
