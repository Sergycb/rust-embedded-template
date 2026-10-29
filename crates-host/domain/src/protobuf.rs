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
mod wiring;

use defmt_or_log::{FormatOrDebug, info, warn};
use micropb::MessageDecode;
use ports::{FrameDuplex, LinkError};
use proto::Request;

use crate::protobuf::codec::{decode_reason, encode};

pub use service::{DeviceInfo, REQUEST_MAX, RESPONSE_MAX, Service};
pub use wiring::ProtobufWiring;

// Узел объявлен рядом со своей задачей, по узлу на транспорт.
//
// `<L>` — параметр фрагмента: канал платы `domain` не видит. `local` на
// канале и обработчике — канал вправе быть `!Send` (периферия, сокет).
// Буфер ответа — память, которую объявил граф (`static [u8; RESPONSE_MAX]`).
supervisor::supervisor_fragment! {
    name: PROTOBUF_FRAG<L>;
    boot: inputs: $crate::protobuf::ProtobufWiring<L>;

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
mod tests;
