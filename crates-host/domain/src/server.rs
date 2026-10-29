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

// {%- if graph != "true" %}
pub use wiring::serve;
// {%- endif %}

use defmt_or_log::{FormatOrDebug, info, warn};
use ports::{FrameLink, LinkError};

// Узел объявлен рядом со своей задачей, по узлу на транспорт.
//
// `<L, H>` — параметры фрагмента: канал платы и обработчик приложения
// `domain` не видит. `local` на обоих — канал вправе быть `!Send`
// (периферия, сокет), обработчик — тоже (ручки к общей карте, клиенты входов
// других узлов).
supervisor::supervisor_fragment! {
    name: SERVER_FRAG<L, H>;
    boot: inputs: struct $crate::server::ServerWiring;

    node resources: [
            /// Кадровый канал — с буфером кадра: не меньше самого длинного
            /// запроса и ответа протокола.
            LINK: local L = inputs.link,
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
pub async fn task<L, H>(mut ctx: ServerCtx<'_, L, H>)
where
    L: FrameLink,
    L::Error: FormatOrDebug,
    H: Handler,
{
    match ctx.run().await {}
}

#[cfg(test)]
mod tests {
    //! Тесты узла протокола: ответы на кадры и отказы канала.

    use ports::LinkError;

    use super::Handler;
    use super::fakes::{Link, serve};

    /// Отвечает кадром наоборот; на пустой кадр не отвечает.
    struct Reverse;

    impl Handler for Reverse {
        fn handle(&mut self, frame: &mut [u8], len: usize) -> impl Future<Output = Option<usize>> {
            frame[..len].reverse();
            std::future::ready((len > 0).then_some(len))
        }
    }

    #[test]
    fn answers_each_frame_and_stays_silent_when_the_handler_is() {
        let mut link = Link::with([Ok(vec![1, 2, 3]), Ok(vec![]), Ok(vec![4, 5])], 8);
        serve(&mut link, &mut Reverse);
        assert_eq!(link.sent, [vec![3, 2, 1], vec![5, 4]]);
    }

    /// Потерянный кадр и закрытый канал узел переживает: следующий кадр —
    /// ответ.
    #[test]
    fn lost_frames_and_a_closed_link_do_not_stop_the_node() {
        let mut link = Link::with(
            [
                Err(LinkError::Lost("помеха")),
                Err(LinkError::Down("кабель вынут")),
                Ok(vec![1, 2]),
            ],
            8,
        );
        serve(&mut link, &mut Reverse);
        assert_eq!(link.sent, [vec![2, 1]]);
    }

    /// Ответ, не ушедший в канал, — отказом или потерей, — узел не
    /// останавливает: он ждёт следующий кадр и отвечает на него.
    #[test]
    fn failed_sends_do_not_stop_the_node() {
        for send in [
            Err(LinkError::Down("обрыв")),
            Err(LinkError::Lost("не влез")),
        ] {
            let mut link = Link::with([Ok(vec![1]), Ok(vec![2])], 8);
            link.send = send;
            serve(&mut link, &mut Reverse);
            assert_eq!(link.sent.len(), 2, "оба кадра обработаны");
        }
    }
}

#[cfg(test)]
pub(crate) mod fakes {
    //! Фейк кадрового канала и прогон узла над ним — для тестов узла и
    //! обработчиков, которые гоняют через него.

    use std::collections::VecDeque;

    use core::pin::pin;
    use core::task::{Context, Waker};
    use ports::{FrameLink, LinkError};

    use super::{Handler, ServerCtx, task};

    /// Канал: отдаёт заранее заготовленные исходы `recv`, а когда они
    /// кончаются, ждёт следующего кадра вечно — как настоящий канал.
    /// Отправленные кадры складывает в `sent`.
    pub(crate) struct Link {
        pub(crate) incoming: VecDeque<Result<Vec<u8>, LinkError<&'static str>>>,
        pub(crate) sent: Vec<Vec<u8>>,
        pub(crate) send: Result<(), LinkError<&'static str>>,
        pub(crate) buffer: Vec<u8>,
    }

    impl Link {
        /// Канал с кадрами `incoming`, исправной отправкой и буфером на
        /// `capacity` байт.
        pub(crate) fn with(
            incoming: impl IntoIterator<Item = Result<Vec<u8>, LinkError<&'static str>>>,
            capacity: usize,
        ) -> Self {
            Self {
                incoming: incoming.into_iter().collect(),
                sent: Vec::new(),
                send: Ok(()),
                buffer: vec![0; capacity],
            }
        }
    }

    impl FrameLink for Link {
        type Error = &'static str;

        fn recv(&mut self) -> impl Future<Output = Result<usize, LinkError<Self::Error>>> {
            let buf = &mut self.buffer;
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

        fn buffer(&mut self) -> &mut [u8] {
            &mut self.buffer
        }

        fn send(&mut self, len: usize) -> impl Future<Output = Result<(), LinkError<Self::Error>>> {
            self.sent.push(self.buffer[..len].to_vec());
            std::future::ready(self.send)
        }
    }

    /// Гоняет узел над `link` и `handler` по всем заготовленным кадрам, пока
    /// он не станет ждать следующий.
    pub(crate) fn serve<H: Handler>(link: &mut Link, handler: &mut H) {
        let ctx = ServerCtx {
            link,
            handler,
            index: 0,
        };
        let mut task = pin!(task(ctx));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(task.as_mut().poll(&mut cx).is_pending(), "ждёт кадр");
    }
}
