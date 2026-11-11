//! Modbus: узел протокола `MODBUS_<транспорт>`, slave уровня PDU и — при
//! выборе — master.
//!
//! Узел принимает кадр, отдаёт его slave'у [`Slave`](crate::modbus::Slave) и
//! отправляет ответ. Обработчик протокола один — slave, — поэтому трейта под
//! него нет: узел зовёт его прямо. Что за транспорт, решает канал
//! ([`ports::FrameLink`]): RTU, MBAP по TCP и UDP; slave про канал не знает.
// {%- if graph == "true" %}
//! Граф даёт узлу имя по протоколу и транспорту (`MODBUS_<транспорт>`).
// {%- endif %}
// {%- if has_protobuf == "true" %}
//!
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
//!
//! Канал строит compose-site (транспорт из `adapters_logic::modbus`, порт
//! `ports::FrameLink`) и отдаёт узлу готовым; здесь про него ничего не
//! известно: кадр, CRC, заголовок MBAP и пропуск битых кадров — дело канала.
//! Данные slave'а — порт [`ports::Registers`]: его реализуют карта проекта
//! ([`EmptyRegisters`](crate::modbus::EmptyRegisters) — её заготовка) и
//! обёртки над ней, а зовёт [`Slave`](crate::modbus::Slave).
// {%- if modbus_master == "true" %}
//! Master — тоже на уровне PDU, поверх транспорта [`ports::ModbusClient`]
//! (из коробки — только RTU над UART, см. `master`), по функции на запрос,
//! без узла: что опрашивать, как часто и куда публиковать, решает приложение
//! в своём узле.
// {%- endif %}
//! Разбор PDU — `rusty-modbus-codec`; кодирование — здесь: оно тривиально, а
//! разбор чужих байт — то место, где решается надёжность.
//!
//! # Почему `rusty-modbus-codec`
//!
//! Кандидаты прогонялись одними и теми же проверками по спецификации
//! (количество вне диапазона, счётчик байт не сходится, coil не
//! 0x0000/0xFF00, коды исключений шлюза, FC15) и фаззингом случайными
//! кадрами с верной CRC:
//!
//! * `rusty-modbus-codec` (MIT, `no_std`, без alloc) — отказы ровно по
//!   спецификации, ни одной паники на 2 млн случайных кадров; только кодек,
//!   без ввода-вывода, поэтому транспорты, slave и master — свои;
//! * `rmodbus` — slave принимает qty=0 и FC16 с несовпадающим счётчиком,
//!   master паникует на ответе с пустыми данными (1384 паники на 1 млн
//!   кадров с верной CRC) — то есть падает от кривого slave'а;
//! * `oxictl` — slave не обрабатывает FC15, отдаёт битый ответ при qty>125,
//!   принимает недопустимое значение coil'а;
//! * `async-modbus` — только клиент, без FC05/15 и без разбора исключений;
//! * `modbus-rs`/`mbus-*` — GPLv3 или коммерческая лицензия; `rs-modbus`,
//!   `rusty-modbus` целиком — требуют `std` (tokio).

// {%- if modbus_master == "true" %}
/// Master: по функции на запрос, ответ сверяется с запросом.
pub mod master;

// {%- endif %}
/// Slave: обработчик кадра, запросы — карте [`Registers`], идентификация —
/// FC43.
mod slave;

/// Карта по умолчанию, общая ручка на несколько узлов и окно OTA.
mod map;

use defmt_or_log::{FormatOrDebug, info, warn};
use ports::FrameLink;

pub use map::{EmptyRegisters, MapMutex, SharedRegisters};
// Трейт карты и представления кадра — порт (`ports::Registers`): у карты
// несколько реализаций, вложенных друг в друга. Здесь — тем же путём, что и
// остальной словарь Modbus узла.
pub use ports::{Bits, BitsMut, Registers, Words, WordsMut};
pub use slave::{PRODUCT, Slave, VENDOR};
// {%- if ota == "true" %}
pub use map::{OTA_BASE, OTA_REGISTERS, OtaRegisters, STATUS_REGISTERS};
// {%- endif %}

// Узел объявлен рядом со своей задачей, по узлу на транспорт.
//
// `<L, R>` — параметры фрагмента: канал платы и карта регистров приложения
// `domain` не видит. `local` на обоих — канал вправе быть `!Send`
// (периферия, сокет), slave — тоже (ручки к общей карте, клиенты входов
// других узлов).
supervisor::supervisor_fragment! {
    name: MODBUS_FRAG<L, R>;
    boot: inputs: struct $crate::modbus::ModbusWiring;

    node resources: [
            /// Кадровый канал — с буфером кадра: не меньше самого длинного
            /// запроса и ответа протокола.
            LINK: local L = inputs.link,
            /// Modbus slave над картой приложения: что ответить на кадр.
            HANDLER: local $crate::modbus::Slave<R> = inputs.handler
        ],
        ctx: $crate::modbus::ModbusCtx,
        task: $crate::modbus::task;
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
pub async fn task<L, R>(ctx: ModbusCtx<'_, L, R>)
where
    L: FrameLink,
    L::Error: FormatOrDebug,
    R: Registers,
{
    let capacity = ctx.link.buffer().len();
    info!("modbus: узел запущен, буфер кадра {} байт", capacity);
    ctx.handler.begin(capacity);
    loop {
        match ctx.link.recv().await {
            Ok(len) => {
                let Some(reply) = ctx.handler.handle(ctx.link.buffer(), len).await else {
                    continue;
                };
                if let Err(err) = ctx.link.send(reply).await {
                    warn!("modbus: ответ не ушёл: {:?}", &err);
                }
            }
            Err(err) => warn!("modbus: кадр не принят: {:?}", err),
        }
    }
}

#[cfg(test)]
pub(crate) mod fake {
    //! Фейки для host-тестов Modbus: транспорт master'а, кадровый канал узла
    //! и карта регистров — все готовы с первого `poll`.

    // {%- if modbus_master == "true" %}
    use core::convert::Infallible;

    // {%- endif %}
    use core::pin::pin;
    use core::task::{Context, Waker};
    use ports::{Exception, FrameLink, LinkError};
    // {%- if modbus_master == "true" %}
    use ports::{MasterError, ModbusClient};
    // {%- endif %}
    use std::collections::VecDeque;

    use super::{ModbusCtx, Slave, task};
    use crate::modbus::{Bits, BitsMut, Registers, Words, WordsMut};

    // {%- if modbus_master == "true" %}
    /// Транспорт master'а: записывает запросы, на первый обмен отвечает
    /// `reply`, без него — концом потока.
    #[derive(Default)]
    pub(super) struct Client {
        reply: Option<Vec<u8>>,
        /// Каждый запрос: адрес (0 — широковещательный) и PDU.
        pub(super) sent: Vec<(u8, Vec<u8>)>,
    }

    impl Client {
        pub(super) fn replying(reply: &[u8]) -> Self {
            Self {
                reply: Some(reply.to_vec()),
                sent: Vec::new(),
            }
        }
    }

    impl ModbusClient for Client {
        type Error = Infallible;

        fn exchange(
            &mut self,
            unit: u8,
            request: &[u8],
            response: &mut [u8],
        ) -> impl Future<Output = Result<usize, MasterError<Infallible>>> {
            self.sent.push((unit, request.to_vec()));
            let result = self.reply.take().map_or(Err(MasterError::Eof), |reply| {
                response[..reply.len()].copy_from_slice(&reply);
                Ok(reply.len())
            });
            std::future::ready(result)
        }

        fn broadcast(
            &mut self,
            request: &[u8],
        ) -> impl Future<Output = Result<(), MasterError<Infallible>>> {
            self.sent.push((0, request.to_vec()));
            std::future::ready(Ok(()))
        }
    }
    // {%- endif %}

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

    /// Карта устройства: 16 coils и 16 holding-регистров; input registers и
    /// discrete inputs не реализованы — на них slave отвечает Illegal Function.
    #[derive(Default)]
    pub(crate) struct Map {
        pub(super) coils: [bool; 16],
        pub(super) holding: [u16; 16],
        /// Каждый вызов записи регистров: начало и значения — по нему тест
        /// видит, что многорегистровая запись пришла одним вызовом.
        pub(super) writes: Vec<(u16, Vec<u16>)>,
        /// Сколько раз карту читали (coils и holding).
        pub(super) reads: usize,
    }

    impl Registers for Map {
        fn read_coils(
            &mut self,
            start: u16,
            mut out: BitsMut<'_>,
        ) -> impl Future<Output = Result<(), Exception>> {
            self.reads += 1;
            std::future::ready(range(start, out.len()).map(|range| {
                for (i, &value) in (0..).zip(&self.coils[range]) {
                    out.set(i, value);
                }
            }))
        }

        fn read_holding(
            &mut self,
            start: u16,
            mut out: WordsMut<'_>,
        ) -> impl Future<Output = Result<(), Exception>> {
            self.reads += 1;
            std::future::ready(range(start, out.len()).map(|range| {
                for (i, &value) in (0..).zip(&self.holding[range]) {
                    out.set(i, value);
                }
            }))
        }

        fn write_coils(
            &mut self,
            start: u16,
            values: Bits<'_>,
        ) -> impl Future<Output = Result<(), Exception>> {
            std::future::ready(range(start, values.len()).map(|range| {
                for (slot, value) in self.coils[range].iter_mut().zip(values.iter()) {
                    *slot = value;
                }
            }))
        }

        fn write_holding(
            &mut self,
            start: u16,
            values: Words<'_>,
        ) -> impl Future<Output = Result<(), Exception>> {
            std::future::ready(range(start, values.len()).map(|range| {
                for (slot, value) in self.holding[range].iter_mut().zip(values.iter()) {
                    *slot = value;
                }
                self.writes.push((start, values.iter().collect()));
            }))
        }
    }

    /// Диапазон `start..start + len` внутри карты из 16 элементов.
    fn range(start: u16, len: u16) -> Result<core::ops::Range<usize>, Exception> {
        let start = usize::from(start);
        let end = start + usize::from(len);
        if end <= 16 {
            Ok(start..end)
        } else {
            Err(Exception::IllegalDataAddress)
        }
    }

    /// Гоняет узел над `link` и slave'ом `handler` по всем заготовленным кадрам, пока
    /// он не станет ждать следующий.
    pub(crate) fn serve<R: Registers>(link: &mut Link, handler: &mut Slave<R>) {
        let ctx = ModbusCtx {
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
