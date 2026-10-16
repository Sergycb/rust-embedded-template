//! Modbus: slave и master на уровне PDU — функция и данные — поверх
//! транспортов [`ports::ModbusServer`] и [`ports::ModbusClient`].
//!
//! Транспорт — RTU поверх UART, TCP или UDP — подставляет compose-site
//! (`adapters::modbus::{Rtu, Tcp, Udp}`); здесь про него ничего не известно:
//! кадр, CRC, заголовок MBAP и пропуск битых кадров — дело транспорта.
//! Данные slave'а — порт [`ports::Registers`], его реализует приложение;
//! обслуживает транспорт узел графа `MODBUS_SLAVE_FRAG` (ниже). Master — по
//! функции на запрос, без узла: что опрашивать, как часто и куда
//! публиковать, решает приложение в своём узле. Разбор PDU —
//! `rusty-modbus-codec`; кодирование — здесь: оно тривиально, а разбор
//! чужих байт — то место, где решается надёжность.
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

/// Master: по функции на запрос, ответ сверяется с запросом.
pub mod master;

/// Карта регистров по умолчанию — пустая, место под карту устройства.
mod registers;

pub use registers::Registers;

#[cfg(test)]
mod fakes;

use defmt_or_log::{FormatOrDebug, info, warn};
use embassy_time::Duration;
use ports::{ModbusServer, ServeError};
use supervisor::policy::{BackoffPolicy, JitterPolicy};
use supervisor::runtime::TaskExit;

/// Политика пауз между перезапусками узла — те же цифры, что у
/// [`ota::BACKOFF`](crate::ota::BACKOFF), и своя константа намеренно:
/// фрагмент самодостаточен.
pub const BACKOFF: BackoffPolicy = BackoffPolicy {
    first: Duration::from_millis(50),
    factor: 2,
    jitter: JitterPolicy::None,
    floor: Duration::from_millis(50),
    max: Duration::from_secs(5),
};

// Вход узла — `ModbusSlaveWiring`: структуру эмитит сам фрагмент (`boot:
// … struct`). Строит её compose-site из полей платы, подставляя типы
// транспорта и данных аргументами фрагмента:
//
// ```ignore
// // `bsp::serial::Serial` и `board.serial` — поток байтов UART платы.
// fragments: [::domain::MODBUS_SLAVE_FRAG<::adapters::modbus::Rtu<bsp::serial::Serial>, ::domain::modbus::Registers> as MODBUS
//                 = ::domain::modbus::ModbusSlaveWiring {
//                     link: ::adapters::modbus::Rtu::new(board.serial),
//                     regs: ::domain::modbus::Registers,
//                     unit: 1,
//                 }];
// ```

/// Узел `MODBUS` (slave): обслуживает транспорт, пока тот жив.
///
/// `TaskExit::Failed` — только при отказе транспорта, конце потока или
/// потерянной границе кадра:
/// всё, что приходит по линии, slave переживает сам. Граф перезапустит узел
/// с backoff'ом, и транспорт вернётся в слот к новому прогону. `Completed`
/// не возвращается никогда.
///
/// Без `watchdog:` намеренно — по правилу [`ota::run`](crate::ota::run):
/// slave законно ждёт запрос сколько угодно, и отметка по таймеру сторожила
/// бы исполнитель, а не работу.
///
/// # Panics
///
/// Если адрес во входе не 1..=247 — ошибка конфигурации, а не линии.
pub async fn run<L, R>(mut ctx: ModbusSlaveCtx<'_, L, R>) -> TaskExit
where
    L: ModbusServer,
    L::Error: FormatOrDebug,
    R: ports::Registers,
{
    ctx.run().await.result
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
                /// Обслуживает линию.
                Serving,
                /// Отказал транспорт: прогон окончен.
                exit Down { result: TaskExit },
            }
            start => begin -> Serving;
            transitions {
                Serving => serve -> Down,
            }
        }
    }

    fn begin(&mut self) -> Serving {
        info!("domain: узел MODBUS запущен, адрес {}", *self.unit);
        Serving
    }

    /// Обслуживает транспорт до конца сессии или отказа.
    ///
    /// Конец потока — не отказ, а конец сессии: TCP-клиент отключился, и
    /// транспорт над `net::TcpServer` на следующем приёме примет нового.
    /// Узел обслуживает его дальше, не выходя: выход `Failed` на каждое
    /// отключение растил бы паузу перезапуска графа до `BACKOFF.max`, и
    /// мастер, открывающий соединение на каждый опрос, ждал бы секунды.
    async fn serve(&mut self, _: Serving) -> Down {
        // Сессии — подряд: каждая ждёт данных транспорта, так что цикл
        // уступает исполнитель, а не крутится.
        loop {
            match slave::serve(self.link, *self.unit, self.regs).await {
                Err(ServeError::Eof) => info!("modbus: сессия закончилась, ждём следующую"),
                Err(err) => {
                    warn!("modbus: отказ транспорта: {:?}", &err);
                    return Down {
                        result: TaskExit::Failed,
                    };
                }
            }
        }
    }
}

// Узел объявлен рядом со своей задачей — как `OTA_FRAG` в `domain::ota`;
// compose-site только перечисляет фрагмент, называет узел (`as MODBUS`) и
// кормит его входом. В граф шаблона узел попадёт по выбору протокола при
// генерации проекта.
//
// `<L, R>` — параметры фрагмента по той же причине, что у `OTA_FRAG`: тип
// ресурсного слота назвать обязан, а транспорт платы и данные приложения
// `domain` не видит. `local` на обоих — по той же причине: транспорт вправе
// быть `!Send` (DMA, периферия), данные приложения — тоже. `UNIT` — обычный
// слот.
supervisor::supervisor_fragment! {
    name: MODBUS_SLAVE_FRAG<L, R>;
    boot: inputs: struct $crate::modbus::ModbusSlaveWiring;

    node deps: [], restart: ::supervisor::policy::RestartPolicy::OnFailure,
        backoff: $crate::modbus::BACKOFF,
        resources: [
            /// Транспорт — `adapters::modbus::{Rtu, Tcp, Udp}` над интерфейсом платы.
            LINK: local L = inputs.link,
            /// Данные устройства — реализация `ports::Registers` (по умолчанию —
            /// пустая карта `domain::modbus::Registers`).
            REGS: local R = inputs.regs,
            /// Адрес slave'а на шине, 1..=247.
            UNIT: u8 = inputs.unit
        ],
        ctx: $crate::modbus::ModbusSlaveCtx,
        task: $crate::modbus::run;
}

#[cfg(test)]
mod tests {
    use super::{ModbusSlaveCtx, run};
    use crate::modbus::fakes::{Map, Server};
    use supervisor::runtime::TaskExit;

    /// Узел отвечает на запросы, конец потока переживает как конец сессии и
    /// выходит `Failed` только при отказе транспорта — граф перезапустит его.
    #[tokio::test]
    async fn serves_sessions_until_transport_fails() {
        let mut link = Server::new(&[(7, &[0x06, 0, 1, 0, 42])]);
        let mut regs = Map::default();
        let ctx = ModbusSlaveCtx {
            link: &mut link,
            regs: &mut regs,
            unit: &mut 7,
            index: 0,
        };
        assert_eq!(run(ctx).await, TaskExit::Failed);
        assert_eq!(regs.holding[1], 42);
        assert_eq!(link.replies, [[0x06, 0, 1, 0, 42]]);
    }
}
