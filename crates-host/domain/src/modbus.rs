//! Modbus RTU: slave и master поверх потока байтов
//! (`embedded_io_async::{Read, Write}`).
//!
//! Данные slave'а — порт [`ports::Registers`], его реализует приложение;
//! обслуживает линию узел графа `MODBUS_SLAVE_FRAG` (ниже). Master — по
//! функции на запрос, без узла: что опрашивать, как часто и куда
//! публиковать, решает приложение в своём узле. Разбор PDU — `rusty-modbus-codec`; кадр
//! RTU, CRC и кодирование — здесь: кодирование тривиально, а разбор чужих
//! байт — то место, где решается надёжность.
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
//!   без ввода-вывода, поэтому кадр, slave и master — свои;
//! * `rmodbus` — slave принимает qty=0 и FC16 с несовпадающим счётчиком,
//!   master паникует на ответе с пустыми данными (1384 паники на 1 млн
//!   кадров с верной CRC) — то есть падает от кривого slave'а;
//! * `oxictl` — slave не обрабатывает FC15, отдаёт битый ответ при qty>125,
//!   принимает недопустимое значение coil'а;
//! * `async-modbus` — только клиент, без FC05/15 и без разбора исключений;
//! * `modbus-rs`/`mbus-*` — GPLv3 или коммерческая лицензия; `rs-modbus`,
//!   `rusty-modbus` целиком — требуют `std` (tokio).
//!
//! Граница кадра в потоке выводится из содержимого (код функции, счётчик
//! байт, CRC), а не из паузы на линии: транспорт паузу не передаёт. На
//! линии точка-точка этого достаточно; на общей шине RS-485 slave видит и
//! ответы других slave'ов, и чужой ответ может на время сбить его с
//! границы, пока CRC не выровняет поток.

/// Кадр RTU: CRC и длина кадра по первым байтам.
mod rtu;

/// Slave: обслуживает линию, отдавая запросы порту [`ports::Registers`].
pub mod slave;

/// Master: по функции на запрос, ответ сверяется с запросом.
pub mod master;

#[cfg(test)]
mod fakes;

use core::fmt::Debug;

use defmt_or_log::{Debug2Format, info, warn};
use embassy_time::Duration;
use embedded_io_async::{Read, Write};
use ports::Registers;
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

/// Что узлу нужно на входе. Строит compose-site из полей платы, подставляя
/// типы транспорта и данных аргументами фрагмента:
///
/// ```ignore
/// fragments: [::domain::MODBUS_SLAVE_FRAG<bsp::Rs485, crate::Registers> as MODBUS
///                 = ::domain::modbus::ModbusSlaveWiring { io: board.rs485, regs: crate::Registers::new(), unit: 1 }];
/// ```
#[derive(Debug)]
pub struct ModbusSlaveWiring<T, R> {
    /// Транспорт — поток байтов линии (`BufferedUart` с DE для RS-485).
    pub io: T,
    /// Данные устройства — реализация [`Registers`] приложения.
    pub regs: R,
    /// Адрес slave'а на шине, 1..=247.
    pub unit: u8,
}

/// Узел `MODBUS` (slave): обслуживает линию, пока жив транспорт.
///
/// `TaskExit::Failed` — только при отказе транспорта или конце потока:
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
pub async fn run<T, R>(mut ctx: ModbusSlaveCtx<'_, T, R>) -> TaskExit
where
    T: Read + Write,
    T::Error: Debug,
    R: Registers,
{
    ctx.run().await.result
}

#[fsm::typestate]
impl<T, R> ModbusSlaveCtx<'_, T, R>
where
    T: Read + Write,
    T::Error: Debug,
    R: Registers,
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

    /// Обслуживает линию до отказа транспорта.
    async fn serve(&mut self, _: Serving) -> Down {
        let Err(err) = slave::serve(self.io, *self.unit, self.regs).await;
        warn!("modbus: отказ транспорта: {:?}", Debug2Format(&err));
        Down {
            result: TaskExit::Failed,
        }
    }
}

// Узел объявлен рядом со своей задачей — как `OTA_FRAG` в `domain::ota`;
// compose-site только перечисляет фрагмент, называет узел (`as MODBUS`) и
// кормит его входом. В граф шаблона узел попадёт по выбору протокола при
// генерации проекта.
//
// `<T, R>` — параметры фрагмента по той же причине, что у `OTA_FRAG`: тип
// ресурсного слота назвать обязан, а транспорт платы и данные приложения
// `domain` не видит. `local` на обоих — по той же причине: транспорт вправе
// быть `!Send` (DMA, периферия), данные приложения — тоже. `UNIT` — обычный
// слот.
supervisor::supervisor_fragment! {
    name: MODBUS_SLAVE_FRAG<T, R>;
    boot: inputs: $crate::modbus::ModbusSlaveWiring<T, R>;

    node deps: [], restart: ::supervisor::policy::RestartPolicy::OnFailure,
        backoff: $crate::modbus::BACKOFF,
        resources: [
            IO: local T = inputs.io,
            REGS: local R = inputs.regs,
            UNIT: u8 = inputs.unit
        ],
        ctx: $crate::modbus::ModbusSlaveCtx,
        task: $crate::modbus::run;
}

#[cfg(test)]
mod tests {
    use super::{ModbusSlaveCtx, run};
    use crate::modbus::fakes::{Duplex, Map, frame};
    use supervisor::runtime::TaskExit;

    /// Узел отвечает на запросы и выходит `Failed`, когда поток кончается, —
    /// граф перезапустит его.
    #[tokio::test]
    async fn serves_until_transport_ends() {
        let mut io = Duplex::new(&frame(7, &[0x06, 0, 1, 0, 42]));
        let mut regs = Map::default();
        let ctx = ModbusSlaveCtx {
            io: &mut io,
            regs: &mut regs,
            unit: &mut 7,
            index: 0,
        };
        assert_eq!(run(ctx).await, TaskExit::Failed);
        assert_eq!(regs.holding[1], 42);
        assert_eq!(io.tx, frame(7, &[0x06, 0, 1, 0, 42]));
    }
}
