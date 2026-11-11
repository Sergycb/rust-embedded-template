//! Узел протокола «запрос — ответ» над кадровым каналом: принимает кадр,
//! отдаёт его Modbus slave'у ([`Slave`](crate::modbus::Slave)) и отправляет
//! ответ.
//!
//! Что за транспорт — решает канал ([`ports::FrameLink`]): RTU, MBAP по TCP и
//! UDP; slave про канал не знает. Обработчик протокола один — slave, —
//! поэтому трейта под него нет: узел зовёт его прямо.
// {%- if graph == "true" %}
//! Граф даёт узлу имя по протоколу и транспорту (`MODBUS_<транспорт>`).
// {%- endif %}
// {%- if has_protobuf == "true" %}
//! У protobuf хоста свой узел, `domain::protobuf`: его канал —
//! [`ports::FrameDuplex`] (брошенный `recv` не теряет кадр), а ответ узел
//! кодирует в свой буфер и отправляет `send_from`, не трогая буфер канала:
//! это задел под второй вход — сообщения по инициативе устройства (его пока
//! нет).
// {%- endif %}
//!
//! Что узел делает с тем, что пришло:
//!
//! * кадр — обработчику; ответ (`Some`) — в канал, без ответа (`None`) —
//!   ничего: чужой адрес, широковещание, кадр без PDU;
//! * кадр потерян или канал отказал — на приёме или на отправке — в лог, и
//!   узел ждёт следующий кадр: восстановить канал и подождать, пока он
//!   вернётся, — дело реализации `FrameLink` на следующем `recv`.

use defmt_or_log::{FormatOrDebug, info, warn};
use ports::{FrameLink, Registers};

// Узел объявлен рядом со своей задачей, по узлу на транспорт.
//
// `<L, R>` — параметры фрагмента: канал платы и карта регистров приложения
// `domain` не видит. `local` на обоих — канал вправе быть `!Send`
// (периферия, сокет), slave — тоже (ручки к общей карте, клиенты входов
// других узлов).
supervisor::supervisor_fragment! {
    name: SERVER_FRAG<L, R>;
    boot: inputs: struct $crate::server::ServerWiring;

    node resources: [
            /// Кадровый канал — с буфером кадра: не меньше самого длинного
            /// запроса и ответа протокола.
            LINK: local L = inputs.link,
            /// Modbus slave над картой приложения: что ответить на кадр.
            HANDLER: local $crate::modbus::Slave<R> = inputs.handler
        ],
        ctx: $crate::server::ServerCtx,
        task: $crate::server::task;
}

/// Узел протокола: обслуживает канал, пока жив, и не выходит никогда.
///
/// Цикл: кадр — обработчику; ответ (`Some`) — в канал, без ответа (`None`) —
/// ничего: чужой адрес, широковещание, кадр без PDU. Кадр потерян или канал
/// отказал — на приёме или на отправке — в лог, и узел ждёт следующий кадр:
/// восстановить канал и подождать, пока он вернётся, — дело реализации
/// `FrameLink` на следующем `recv`.
///
/// Без `watchdog:` намеренно: узел законно
/// ждёт запрос сколько угодно, и отметка по таймеру сторожила бы
/// исполнитель, а не работу.
pub async fn task<L, R>(ctx: ServerCtx<'_, L, R>)
where
    L: FrameLink,
    L::Error: FormatOrDebug,
    R: Registers,
{
    let capacity = ctx.link.buffer().len();
    info!("server: узел запущен, буфер кадра {} байт", capacity);
    ctx.handler.begin(capacity);
    loop {
        match ctx.link.recv().await {
            Ok(len) => {
                let Some(reply) = ctx.handler.handle(ctx.link.buffer(), len).await else {
                    continue;
                };
                if let Err(err) = ctx.link.send(reply).await {
                    warn!("server: ответ не ушёл: {:?}", &err);
                }
            }
            Err(err) => warn!("server: кадр не принят: {:?}", err),
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! Фейк кадрового канала и прогон узла над ним — для тестов узла и
    //! обработчиков, которые гоняют через него.

    use std::collections::VecDeque;

    use core::pin::pin;
    use core::task::{Context, Waker};
    use ports::{FrameLink, LinkError};

    use ports::Registers;

    use super::{ServerCtx, task};
    use crate::modbus::Slave;

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

    /// Гоняет узел над `link` и slave'ом `handler` по всем заготовленным кадрам, пока
    /// он не станет ждать следующий.
    pub(crate) fn serve<R: Registers>(link: &mut Link, handler: &mut Slave<R>) {
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
    //! Тесты узла протокола: ответы на кадры и отказы канала — над настоящим
    //! slave'ом и картой в памяти.

    use ports::LinkError;

    use super::fake::{Link, serve};
    use crate::modbus::Slave;
    use crate::modbus::fake::Map;

    /// Ответ slave'а 1 на [`read`]: регистр 0 карты — ноль.
    const REPLY: [u8; 5] = [1, 0x03, 2, 0, 0];

    /// Чтение одного holding-регистра у slave'а `unit` — кадр «адрес и PDU».
    fn read(unit: u8) -> Vec<u8> {
        vec![unit, 0x03, 0, 0, 0, 1]
    }

    fn slave() -> Slave<Map> {
        Slave::new(1, Map::default(), "1.2.3")
    }

    #[test]
    fn answers_each_frame_and_stays_silent_when_the_slave_is() {
        // Второй кадр — чужому адресу: slave молчит, и узел не отправляет
        // ничего.
        let mut link = Link::with([Ok(read(1)), Ok(read(2)), Ok(read(1))], 260);
        serve(&mut link, &mut slave());
        assert_eq!(link.sent, [REPLY.to_vec(), REPLY.to_vec()]);
    }

    /// Потерянный кадр и закрытый канал узел переживает: следующий кадр —
    /// ответ.
    #[test]
    fn lost_frames_and_a_closed_link_do_not_stop_the_node() {
        let mut link = Link::with(
            [
                Err(LinkError::Lost("помеха")),
                Err(LinkError::Down("кабель вынут")),
                Ok(read(1)),
            ],
            260,
        );
        serve(&mut link, &mut slave());
        assert_eq!(link.sent, [REPLY.to_vec()]);
    }

    /// Ответ, не ушедший в канал, — отказом или потерей, — узел не
    /// останавливает: он ждёт следующий кадр и отвечает на него.
    #[test]
    fn failed_sends_do_not_stop_the_node() {
        for send in [
            Err(LinkError::Down("обрыв")),
            Err(LinkError::Lost("не влез")),
        ] {
            let mut link = Link::with([Ok(read(1)), Ok(read(1))], 260);
            link.send = send;
            serve(&mut link, &mut slave());
            assert_eq!(link.sent.len(), 2, "оба кадра обработаны");
        }
    }
}
