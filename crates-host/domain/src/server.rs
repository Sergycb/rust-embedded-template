//! Узел протокола «запрос — ответ» над кадровым каналом: принимает кадр,
//! отдаёт его обработчику и отправляет ответ.
//!
//! Что за протокол — решает обработчик ([`Handler`](crate::server::Handler): Modbus slave
//! `modbus::Slave`), что за транспорт — канал ([`ports::FrameLink`]). Ни тот,
//! ни другой про второго не знает. Обработчик — не порт: его реализует логика
//! `domain`, а не адаптер, поэтому трейт живёт здесь, рядом с узлом.
// {%- if graph == "true" %}
//! Граф даёт узлу имя по протоколу и транспорту (`MODBUS_<транспорт>`).
// {%- endif %}
// {%- if has_protobuf == "true" %}
//! У protobuf хоста свой узел, `domain::protobuf`: он не ждёт исхода команд
//! обновления на месте.
// {%- endif %}
//!
//! Что узел делает с тем, что пришло:
//!
//! * кадр — обработчику; ответ (`Some`) — в канал, без ответа (`None`) —
//!   ничего: чужой адрес, широковещание, ответ не поместился;
//! * кадр потерян или канал отказал — на приёме или на отправке — в лог, и
//!   узел ждёт следующий кадр: восстановить канал и подождать, пока он
//!   вернётся, — дело реализации `FrameLink` на следующем `recv`.

mod wiring;

/// Фейк канала для тестов узла и обработчиков, которые гоняют их через узел.
#[cfg(test)]
pub(crate) mod fakes;

pub use wiring::ServerWiring;
// {%- if graph != "true" %}
pub use wiring::serve;
// {%- endif %}

use defmt_or_log::{FormatOrDebug, info, warn};
use ports::{FrameLink, LinkError};
use supervisor::runtime::TaskExit;

// Узел объявлен рядом со своей задачей, по узлу на транспорт.
//
// `<L, H>` — параметры фрагмента: канал платы и обработчик приложения
// `domain` не видит. `local` на обоих — канал вправе быть `!Send`
// (периферия, сокет), обработчик — тоже (ручки к общей карте, клиенты входов
// других узлов).
supervisor::supervisor_fragment! {
    name: SERVER_FRAG<L, H>;
    boot: inputs: $crate::server::ServerWiring<L, H>;

    node deps: [], restart: Never,
        resources: [
            /// Кадровый канал — с буфером кадра: не меньше самого длинного
            /// запроса и ответа протокола. Собирает его узел — из рычага
            /// платы и памяти приложения.
            LINK: local L = <L as $crate::Assemble>::assemble(inputs.source, inputs.buffers),
            /// Протокол над каналом: что ответить на кадр.
            HANDLER: local H = inputs.handler
        ],
        ctx: $crate::server::ServerCtx,
        task: $crate::server::task;
}

/// Что делать с кадром, принятым каналом `FrameLink`: протокол поверх
/// кадров — разбор запроса, ответ.
///
/// Обработчик — чистая логика: байты запроса на входе, байты ответа на
/// выходе, без ввода-вывода. Узел принимает кадр, отдаёт его обработчику и
/// отправляет ответ, — поэтому один узел обслуживает любой протокол над любым
/// каналом.
#[expect(
    async_fn_in_trait,
    reason = "один исполнитель embassy: границы `Send` у future не нужно"
)]
pub trait Handler {
    /// Проверка при старте узла: `capacity` — размер буфера канала. Обработчик,
    /// которому заранее известен самый длинный ответ, предупреждает здесь,
    /// если он не поместится.
    fn begin(&mut self, capacity: usize) {
        let _ = capacity;
    }

    /// Отвечает на кадр `frame[..len]`: кладёт ответ в начало того же `frame`
    /// и возвращает его длину; `None` — не отвечать (чужой адрес,
    /// широковещательный запрос, ответ не поместился).
    ///
    /// Буфер один на запрос и ответ — тот же, что у канала: запрос, который
    /// нужен и во время записи ответа, обработчик копирует себе сам.
    async fn handle(&mut self, frame: &mut [u8], len: usize) -> Option<usize>;
}

#[fsm::typestate]
impl<L, H> ServerCtx<'_, L, H>
where
    L: FrameLink,
    L::Error: FormatOrDebug,
    H: Handler,
{
    fsm::machine! {
        machine Server mod edges {
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
        let capacity = self.link.buffer().len();
        info!(
            "domain: узел протокола запущен, буфер кадра {} байт",
            capacity
        );
        self.handler.begin(capacity);
        Listening
    }

    /// Отдаёт кадр обработчику и отправляет ответ, если он есть.
    async fn serve(&mut self, len: usize) {
        let Some(reply) = self.handler.handle(self.link.buffer(), len).await else {
            return;
        };
        if let Err(err) = self.link.send(reply).await {
            warn!("server: ответ не ушёл: {:?}", &err);
        }
    }

    /// Кадр потерян или канал отказал — в лог; узел ждёт следующий кадр.
    #[expect(
        clippy::unused_self,
        reason = "сигнатуру действия задаёт таблица `#[fsm::typestate]`"
    )]
    fn failed(&mut self, err: &LinkError<L::Error>) {
        warn!("server: кадр не принят: {:?}", err);
    }
}

/// Узел протокола: обслуживает канал, пока жив, и не выходит никогда.
///
/// Без `watchdog:` намеренно: узел законно
/// ждёт запрос сколько угодно, и отметка по таймеру сторожила бы
/// исполнитель, а не работу.
pub async fn task<L, H>(mut ctx: ServerCtx<'_, L, H>) -> TaskExit
where
    L: FrameLink,
    L::Error: FormatOrDebug,
    H: Handler,
{
    match ctx.run().await {}
}

#[cfg(test)]
mod tests;
