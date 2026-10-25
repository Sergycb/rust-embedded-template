//! Узел протокола «запрос — ответ» над кадровым каналом: принимает кадр,
//! отдаёт его обработчику и отправляет ответ.
//!
//! Что за протокол — решает обработчик ([`ports::Handler`]: Modbus slave
//! `modbus::Slave`), что за транспорт — канал ([`ports::FrameLink`]). Ни тот,
//! ни другой про второго не знает.
// {%- if graph == "true" %}
//! Граф даёт узлу имя по протоколу и транспорту (`MODBUS_<транспорт>`).
// {%- endif %}
// {%- if has_protobuf == "true" %}
//! У protobuf хоста свой узел, `domain::host`: он не ждёт исхода команд
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

use defmt_or_log::{FormatOrDebug, info, warn};
use ports::{Assemble, FrameLink, Handler, LinkError};
use supervisor::runtime::TaskExit;

/// Вход узла: из чего собрать канал `L` и чем отвечать.
///
/// Канал собирает узел (`L::assemble(source, buffers)` — порт
/// [`Assemble`]), а compose-site только раздаёт: рычаг платы, ссылки на
// {%- if graph == "true" %}
/// статики приложения и обработчик. Какой канал под каким рычагом — тип `L`,
/// его выбирает генерация параметром фрагмента (`<вид>`/`<транспорт>` — как в
/// `crates-cross/app/src/graph.rs`):
///
/// ```ignore
/// fragments: [::domain::SERVER_FRAG<crate::link::<вид>::<транспорт>::Link, ::domain::modbus::Slave<crate::modbus::Handle>>
///                 as MODBUS_<ТРАНСПОРТ> = ::domain::server::ServerWiring {
///                     source: board.<транспорт>,
///                     buffers: crate::link::<вид>::<транспорт>::buffers(),
///                     handler: ::domain::modbus::Slave::new(1, crate::modbus::handle()),
///                 }];
/// ```
// {%- else %}
/// статики приложения и обработчик. Какой канал под каким рычагом — тип `L`,
/// его выбирает генерация в задаче `crates-cross/app/src/tasks/modbus.rs`.
// {%- endif %}
///
/// Буфер кадра — у канала (`FrameLink::buffer`), а не у узла: его размер
/// задаёт протокол над каналом, и канал собирается вместе с ним.
///
/// Написана руками, а не выведена фрагментом (`boot: … struct`): слот
/// канала инициализируется сборкой из двух полей, а не чтением одного.
#[derive(Debug)]
pub struct ServerWiring<L: Assemble, H> {
    /// Рычаг платы, над которым собирается канал.
    pub source: L::Source,
    /// Память канала от приложения.
    pub buffers: L::Buffers,
    /// Протокол над каналом: что ответить на кадр.
    pub handler: H,
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

// {%- if graph != "true" %}
/// Узел протокола: собирает канал из `wiring` и обслуживает его автоматом
/// [`task`], не выходя никогда.
pub async fn serve<L, H>(wiring: ServerWiring<L, H>) -> TaskExit
where
    L: Assemble + FrameLink,
    L::Error: FormatOrDebug,
    H: Handler,
{
    let mut link = L::assemble(wiring.source, wiring.buffers);
    let mut handler = wiring.handler;
    task(ServerCtx {
        link: &mut link,
        handler: &mut handler,
        index: 0,
    })
    .await
}
// {%- endif %}

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

// Узел объявлен рядом со своей задачей — как `CPU_LOAD_FRAG`; в граф шаблона
// попадёт по узлу на транспорт с протоколом, выбранным при генерации.
//
// `<L, H>` — параметры фрагмента: канал платы и обработчик приложения
// `domain` в графе не видит. `local` на обоих — канал вправе быть `!Send`
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

/// Фейк канала для тестов узла и обработчиков, которые гоняют их через узел.
#[cfg(test)]
pub(crate) mod fakes {
    use std::collections::VecDeque;

    use core::pin::pin;
    use core::task::{Context, Waker};
    use ports::{FrameLink, Handler, LinkError};

    use super::{ServerCtx, task};

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

#[cfg(test)]
mod tests {
    use ports::{Handler, LinkError};

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
