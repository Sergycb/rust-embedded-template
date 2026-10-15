//! Фейки для host-тестов Modbus: транспорты на уровне PDU и карта регистров.
//!
//! Ни один не ждёт по-настоящему — их future готовы с первого `poll`, —
//! поэтому тестам хватает однопоточного `#[tokio::test]`.

use core::convert::Infallible;
use std::collections::VecDeque;

use ports::{
    Bits, BitsMut, Exception, MasterError, ModbusClient, ModbusServer, Received, Registers,
    ServeError, Words, WordsMut,
};

/// Транспорт master'а: записывает запросы, на первый обмен отвечает
/// `reply`, без него — концом потока.
#[derive(Default)]
pub(crate) struct Client {
    reply: Option<Vec<u8>>,
    /// Каждый запрос: адрес (0 — широковещательный) и PDU.
    pub(crate) sent: Vec<(u8, Vec<u8>)>,
}

impl Client {
    pub(crate) fn replying(reply: &[u8]) -> Self {
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

/// Транспорт slave'а: отдаёт заранее заданные запросы, затем — конец
/// потока, а после него — отказ (`Framing`): так узел, переживающий конец
/// сессии, в тесте всё же заканчивается. Ответы копит в `replies`.
pub(crate) struct Server {
    incoming: VecDeque<(u8, Vec<u8>)>,
    ended: bool,
    pub(crate) replies: Vec<Vec<u8>>,
}

impl Server {
    /// Запросы — адрес и PDU.
    pub(crate) fn new(requests: &[(u8, &[u8])]) -> Self {
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
                Ok(Received {
                    unit,
                    len: pdu.len(),
                })
            }
            None if std::mem::replace(&mut self.ended, true) => Err(ServeError::Framing),
            None => Err(ServeError::Eof),
        };
        std::future::ready(result)
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
pub(crate) struct Map {
    pub(crate) coils: [bool; 16],
    pub(crate) holding: [u16; 16],
    /// Каждый вызов записи регистров: начало и значения — по нему тест
    /// видит, что многорегистровая запись пришла одним вызовом.
    pub(crate) writes: Vec<(u16, Vec<u16>)>,
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
