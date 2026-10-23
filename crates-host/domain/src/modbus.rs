//! Modbus: slave на уровне PDU — функция и данные — обработчиком кадра
//! «адрес и PDU» ([`Slave`](crate::modbus::Slave)) для узла протокола `domain::server`.
// {%- if modbus_master == "true" %}
//! Master — тоже на уровне PDU, поверх транспорта [`ports::ModbusClient`].
// {%- endif %}
//!
//! Канал — RTU поверх UART, TCP или UDP — выбирает типом compose-site, а
//! собирает узел (`adapters::modbus::{Rtu, Tcp, Udp}`, порты `ports::FrameLink`
//! и `ports::Assemble`); здесь про
//! него ничего не известно: кадр, CRC, заголовок MBAP и пропуск битых кадров
//! — дело канала. Данные slave'а — порт [`ports::Registers`], его реализует
//! приложение; обслуживает канал узел протокола `domain::server::SERVER_FRAG`
//! с обработчиком [`Slave`](crate::modbus::Slave).
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

/// Slave: обработчик кадра, запросы — порту [`ports::Registers`].
mod slave;

// {%- if modbus_master == "true" %}
/// Master: по функции на запрос, ответ сверяется с запросом.
pub mod master;

// {%- endif %}

/// Карта регистров по умолчанию — пустая, место под карту устройства.
mod registers;

/// Одна карта регистров на несколько узлов (по узлу на транспорт).
mod shared;
// {%- if ota == "true" %}

/// Окно OTA в карте регистров: команды обновления записями FC16.
mod ota;
// {%- endif %}

pub use registers::EmptyRegisters;
pub use shared::{MapMutex, SharedRegisters};
pub use slave::Slave;
// {%- if ota == "true" %}
pub use ota::{OTA_BASE, OTA_REGISTERS, OtaRegisters, STATUS_REGISTERS};
// {%- endif %}

#[cfg(test)]
mod fakes {
    //! Фейки для host-тестов Modbus: карта регистров и транспорт master'а на
    //! уровне PDU — оба готовы с первого `poll`.

    // {%- if modbus_master == "true" %}
    use core::convert::Infallible;

    // {%- endif %}
    use ports::{Bits, BitsMut, Exception, Registers, Words, WordsMut};
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
}
