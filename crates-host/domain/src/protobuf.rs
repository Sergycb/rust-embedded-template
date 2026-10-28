//! Узел `PROTOBUF`: сообщения protobuf от хоста над кадровым каналом.
// {%- if graph == "true" %}
//! В графе — узел `PROTOBUF_<транспорт>`.
// {%- endif %}
//!
//! Единственное место, где сообщения схемы превращаются в типы приложения,
//! — обработчик [`ProtobufService`](ports::ProtobufService) приложения: в задачи
//! уходят проверенные значения своих типов. Узел только разбирает запрос из
//! кадра и кодирует ответ. Схему он не знает: сообщения — параметры
//! (`micropb::MessageDecode`/`MessageEncode`), пример схемы — крейт
//! `crates-host/proto`.
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
//! * кадр не разобрался как запрос — ответ
//!   [`ProtobufService::malformed`](ports::ProtobufService::malformed);
//! * {% if ota == "true" %}команда обновления (`ProtobufService::route`) — узлу OTA на вход `ASK`, не
//!   дожидаясь исхода: канал обслуживается дальше, а исход, когда придёт,
//!   уходит хосту ответом с тем же `request_id` — ответы на другие запросы
//!   могут его обогнать;
//! * {% endif %}остальное — [`ProtobufService::handle`](ports::ProtobufService::handle) и ответ;
//! * ответ не помещается в буфер — в лог и без ответа, хост получит таймаут;
//!   что буфер мал для какого-то ответа схемы, узел предупреждает уже при
//!   старте.
//!
//! Ответ кодируется в свой буфер узла и уходит `send_from`: в буфере канала
//! может лежать недочитанный кадр — его приём бросила гонка с исходом.
//!
//! Сообщения по инициативе устройства (события, телеметрия) — второй вход
//! узла; появятся по вопросу при генерации проекта.

mod codec;
// Обработчик по умолчанию над схемой `crates-host/proto` — только в проекте с
// протоколом protobuf: без него крейта схемы в проекте нет вовсе.
/// Обработчик по умолчанию над примером схемы `crates-host/proto`.
mod service;
mod wiring;

use defmt_or_log::{FormatOrDebug, info, warn};
use micropb::{MessageDecode, MessageEncode};
use ports::{FrameDuplex, LinkError, ProtobufService};
// {%- if ota == "true" %}
use ports::Route;
// {%- endif %}
use supervisor::runtime::TaskExit;

use crate::protobuf::codec::{decode_reason, encode};

pub use service::{DeviceInfo, REQUEST_MAX, RESPONSE_MAX, Service};
pub use wiring::ProtobufWiring;

// Узел объявлен рядом со своей задачей, по узлу на транспорт.
//
// `<L, S>` — параметры фрагмента: канал платы и обработчик приложения
// `domain` не видит. `local` на обоих — канал вправе быть `!Send`
// (периферия, сокет), обработчик — тоже. Буфер ответа — ссылка на статик
// приложения (`crate::link::<вид>`): его размер — решение приложения.
supervisor::supervisor_fragment! {
    name: PROTOBUF_FRAG<L, S>;
    boot: inputs: $crate::protobuf::ProtobufWiring<L, S>;

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
        ctx: $crate::protobuf::ProtobufCtx,
        task: $crate::protobuf::task;
}

#[fsm::typestate]
impl<L, S> ProtobufCtx<'_, L, S>
where
    L: FrameDuplex,
    L::Error: FormatOrDebug,
    S: ProtobufService,
    S::Request: MessageDecode + Default,
    S::Response: MessageEncode,
{
    fsm::machine! {
        machine Protobuf mod edges {
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
        info!(
            "domain: узел PROTOBUF запущен, буфер ответа {} байт",
            capacity
        );
        if let Ok(max) = S::Response::MAX_SIZE
            && capacity < max
        {
            warn!(
                "protobuf: буфер ответа {} байт, а самый длинный ответ схемы — {}: такие ответы не уйдут",
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
                warn!("protobuf: кадр не разобрался: {}", decode_reason(&err));
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
                "protobuf: команда обновления {} отвергнута: прежняя ещё выполняется",
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
            warn!("protobuf: узел OTA не ответил");
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
        warn!("protobuf: кадр не принят: {:?}", err);
    }

    /// Ответ — в буфер ответа и в канал; не поместился или не ушёл — в лог.
    async fn respond(&mut self, response: &S::Response) {
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
pub async fn task<L, S>(mut ctx: ProtobufCtx<'_, L, S>) -> TaskExit
where
    L: FrameDuplex,
    L::Error: FormatOrDebug,
    S: ProtobufService,
    S::Request: MessageDecode + Default,
    S::Response: MessageEncode,
{
    match ctx.run().await {}
}

#[cfg(test)]
mod tests;
