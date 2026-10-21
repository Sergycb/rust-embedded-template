//! Modbus: slave на уровне PDU — функция и данные — поверх транспорта
//! [`ports::ModbusServer`].
// {%- if modbus_master == "true" %}
//! Master — тоже на уровне PDU, поверх транспорта [`ports::ModbusClient`].
// {%- endif %}
//!
//! Транспорт — RTU поверх UART, TCP или UDP — подставляет compose-site
//! (`adapters::modbus::{Rtu, Tcp, Udp}`); здесь про него ничего не известно:
//! кадр, CRC, заголовок MBAP и пропуск битых кадров — дело транспорта.
//! Данные slave'а — порт [`ports::Registers`], его реализует приложение;
//! обслуживает транспорт узел графа `MODBUS_SLAVE_FRAG` (ниже).
// {%- if modbus_master == "true" %}
//! Master — по функции на запрос, без узла: что опрашивать, как часто и куда
//! публиковать, решает приложение в своём узле.
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

/// Slave: обслуживает транспорт, отдавая запросы порту [`ports::Registers`].
pub mod slave;

// {%- if modbus_master == "true" %}
/// Master: по функции на запрос, ответ сверяется с запросом.
pub mod master;

// {%- endif %}

/// Карта регистров по умолчанию — пустая, место под карту устройства.
mod registers;

pub use registers::EmptyRegisters;

/// Одна карта регистров на несколько узлов (по узлу на транспорт).
mod shared;

pub use shared::{MapMutex, SharedRegisters};
// {%- if ota == "true" %}

/// Окно OTA в карте регистров: команды обновления записями FC16.
mod ota;

pub use ota::{OTA_BASE, OTA_REGISTERS, OtaRegisters, STATUS_REGISTERS};
// {%- endif %}

use defmt_or_log::{FormatOrDebug, info, warn};
use ports::{ModbusServer, ServeError};
use supervisor::runtime::TaskExit;

// Вход узла — `ModbusSlaveWiring`: структуру эмитит сам фрагмент (`boot:
// … struct`). Строит её compose-site из полей платы, подставляя типы
// транспорта и данных аргументами фрагмента:
//
// ```ignore
// // `parts.board.usart1` — поток байтов UART платы; транспорт над ним собирает
// // `crate::link::usart1` (`app`). `parts.modbus` — ручка карты, общей у всех
// // узлов Modbus (`crate::modbus` в `app`; карта по умолчанию — `EmptyRegisters`).
// fragments: [::domain::MODBUS_SLAVE_FRAG<crate::link::usart1::Link, crate::modbus::Handle> as MODBUS_USART1
//                 = ::domain::modbus::ModbusSlaveWiring {
//                     link: crate::link::usart1::new(parts.board.usart1),
//                     regs: parts.modbus,
//                     unit: 1,
//                 }];
// ```

/// Узел `MODBUS` (slave): обслуживает транспорт и не выходит никогда.
///
/// Всё, что приходит по линии, slave переживает сам, а отказ транспорта
/// (потерянная граница кадра, ошибка потока) — в лог и следующая сессия:
/// узел не падает, и графу перезапускать нечего. Ждать после отказа —
/// забота транспорта (`ports::ModbusServer`).
///
/// Без `watchdog:` намеренно — по правилу `heartbeat::task`:
/// slave законно ждёт запрос сколько угодно, и отметка по таймеру сторожила
/// бы исполнитель, а не работу.
///
/// # Panics
///
/// Если адрес во входе не 1..=247 — ошибка конфигурации, а не линии.
pub async fn task<L, R>(mut ctx: ModbusSlaveCtx<'_, L, R>) -> TaskExit
where
    L: ModbusServer,
    L::Error: FormatOrDebug,
    R: ports::Registers,
{
    match ctx.run().await {}
}

#[fsm::typestate]
impl<L, R> ModbusSlaveCtx<'_, L, R>
where
    L: ModbusServer,
    L::Error: FormatOrDebug,
    R: ports::Registers,
{
    fsm::machine! {
        machine ModbusSlave mod edges {
            states {
                /// Обслуживает линию: сессия за сессией.
                Serving,
            }
            start => begin -> Serving;
            listen {
                Serving => session: slave::serve(self.link, *self.unit, self.regs),
            }
            transitions {
                Serving + session(Err(ServeError::Eof)) => ended  -> self,
                Serving + session(Err(ref err))         => failed -> self,
            }
        }
    }

    fn begin(&mut self) -> Serving {
        info!("domain: узел MODBUS запущен, адрес {}", *self.unit);
        Serving
    }

    /// Конец потока — не отказ, а конец сессии: TCP-клиент отключился, и
    /// транспорт над `net::TcpServer` на следующем приёме примет нового.
    #[expect(
        clippy::unused_self,
        reason = "сигнатуру действия задаёт таблица `#[fsm::typestate]`"
    )]
    fn ended(&mut self) {
        info!("modbus: сессия закончилась, ждём следующую");
    }

    /// Отказ транспорта — в лог; следующая сессия начинается сразу.
    #[expect(
        clippy::unused_self,
        reason = "сигнатуру действия задаёт таблица `#[fsm::typestate]`"
    )]
    fn failed(&mut self, err: &ServeError<L::Error>) {
        warn!("modbus: отказ транспорта: {:?}", err);
    }
}

// Узел объявлен рядом со своей задачей — как `CPU_LOAD_FRAG` в `domain::cpu_load`;
// compose-site только перечисляет фрагмент, называет узел (`as MODBUS`) и
// кормит его входом. В граф шаблона узел попадёт по выбору протокола при
// генерации проекта.
//
// `<L, R>` — параметры фрагмента: тип
// ресурсного слота назвать обязан, а транспорт платы и данные приложения
// `domain` не видит. `local` на обоих — по той же причине: транспорт вправе
// быть `!Send` (DMA, периферия), данные приложения — тоже. `UNIT` — обычный
// слот.
supervisor::supervisor_fragment! {
    name: MODBUS_SLAVE_FRAG<L, R>;
    boot: inputs: struct $crate::modbus::ModbusSlaveWiring;

    node deps: [], restart: Never,
        resources: [
            /// Транспорт — `adapters::modbus::{Rtu, Tcp, Udp}` над интерфейсом платы.
            LINK: local L = inputs.link,
            /// Данные устройства — реализация `ports::Registers`; в `app` —
            /// ручка `SharedRegisters` карты, общей у всех узлов Modbus
            /// (`crate::modbus`; карта по умолчанию — `EmptyRegisters`).
            REGS: local R = inputs.regs,
            /// Адрес slave'а на шине, 1..=247.
            UNIT: u8 = inputs.unit
        ],
        ctx: $crate::modbus::ModbusSlaveCtx,
        task: $crate::modbus::task;
}

#[cfg(test)]
mod tests {
    use core::pin::pin;
    use core::task::{Context, Waker};

    use super::{ModbusSlaveCtx, task};
    use crate::modbus::fakes::{Map, Server};

    /// Узел отвечает на запросы, а конец потока переживает как конец сессии:
    /// не выходит, а ждёт следующую.
    #[test]
    fn serves_session_after_session() {
        let mut link = Server::new(&[(7, &[0x06, 0, 1, 0, 42])]);
        let mut regs = Map::default();
        {
            let ctx = ModbusSlaveCtx {
                link: &mut link,
                regs: &mut regs,
                unit: &mut 7,
                index: 0,
            };
            let mut task = pin!(task(ctx));
            let mut cx = Context::from_waker(Waker::noop());
            assert!(
                task.as_mut().poll(&mut cx).is_pending(),
                "ждёт следующую сессию"
            );
        }
        assert_eq!(regs.holding[1], 42);
        assert_eq!(link.replies, [[0x06, 0, 1, 0, 42]]);
    }
}

#[cfg(test)]
mod fakes {
    //! Фейки для host-тестов Modbus: транспорты на уровне PDU и карта регистров.
    //!
    //! Ждёт по-настоящему только транспорт slave'а, когда сценарий кончился, —
    //! как настоящий транспорт ждёт следующего запроса; остальные готовы с
    //! первого `poll`.

    use core::convert::Infallible;
    use std::collections::VecDeque;

    use ports::{
        Bits, BitsMut, Exception, ModbusServer, Received, Registers, ServeError, Words, WordsMut,
    };
    // {%- if modbus_master == "true" %}
    use ports::{MasterError, ModbusClient};
    // {%- endif %}

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

    /// Транспорт slave'а: отдаёт заранее заданные запросы, затем — конец
    /// потока, а после него ждёт следующего запроса вечно, как настоящий
    /// транспорт. Ответы копит в `replies`.
    pub(super) struct Server {
        incoming: VecDeque<(u8, Vec<u8>)>,
        ended: bool,
        pub(super) replies: Vec<Vec<u8>>,
    }

    impl Server {
        /// Запросы — адрес и PDU.
        pub(super) fn new(requests: &[(u8, &[u8])]) -> Self {
            Self {
                incoming: requests.iter().map(|&(u, p)| (u, p.to_vec())).collect(),
                ended: false,
                replies: Vec::new(),
            }
        }
    }

    impl ModbusServer for Server {
        type Error = Infallible;

        fn recv(
            &mut self,
            buf: &mut [u8],
        ) -> impl Future<Output = Result<Received, ServeError<Infallible>>> {
            let result = match self.incoming.pop_front() {
                Some((unit, pdu)) => {
                    buf[..pdu.len()].copy_from_slice(&pdu);
                    Some(Ok(Received {
                        unit,
                        len: pdu.len(),
                    }))
                }
                None if std::mem::replace(&mut self.ended, true) => None,
                None => Some(Err(ServeError::Eof)),
            };
            async move {
                match result {
                    Some(result) => result,
                    None => std::future::pending().await,
                }
            }
        }

        fn reply(
            &mut self,
            response: &[u8],
        ) -> impl Future<Output = Result<(), ServeError<Infallible>>> {
            self.replies.push(response.to_vec());
            std::future::ready(Ok(()))
        }
    }

    /// Карта устройства: 16 coils и 16 holding-регистров; input registers и
    /// discrete inputs не реализованы — на них slave отвечает Illegal Function.
    #[derive(Default)]
    pub(super) struct Map {
        pub(super) coils: [bool; 16],
        pub(super) holding: [u16; 16],
        /// Каждый вызов записи регистров: начало и значения — по нему тест
        /// видит, что многорегистровая запись пришла одним вызовом.
        pub(super) writes: Vec<(u16, Vec<u16>)>,
        /// Сколько раз карту читали (coils и holding).
        pub(super) reads: usize,
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
}
