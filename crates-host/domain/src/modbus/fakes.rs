//! Фейки для host-тестов Modbus: поток байтов и карта регистров.
//!
//! Ни один не ждёт по-настоящему — их future готовы с первого `poll`, —
//! поэтому тестам хватает однопоточного `#[tokio::test]`.

use core::convert::Infallible;

use embedded_io_async::{ErrorType, Read, ReadReady, Write};
use ports::{Bits, BitsMut, Exception, Registers, Words, WordsMut};

use super::rtu::seal;

/// Кадр RTU: адрес `unit`, `pdu` и CRC.
pub(crate) fn frame(unit: u8, pdu: &[u8]) -> Vec<u8> {
    let mut buf = [&[unit][..], pdu, &[0, 0]].concat();
    let len = buf.len() - 2;
    seal(&mut buf, len);
    buf
}

/// Поток: читает заранее записанные байты, пишет в `tx`. Когда читать
/// нечего, `read` отдаёт 0 — конец потока.
pub(crate) struct Duplex {
    rx: Vec<u8>,
    /// Сколько из `rx` уже прочитано.
    pos: usize,
    /// Ответ, который дописывается к входу только после первой записи — как
    /// на линии, где slave отвечает на запрос, а не до него.
    reply: Option<Vec<u8>>,
    pub(crate) tx: Vec<u8>,
}

impl Duplex {
    /// Байты читаемы сразу — вход slave'а.
    pub(crate) fn new(rx: &[u8]) -> Self {
        Self {
            rx: rx.to_vec(),
            pos: 0,
            reply: None,
            tx: Vec::new(),
        }
    }

    /// `reply` читаем только после того, как master отправил запрос.
    pub(crate) fn replying(reply: &[u8]) -> Self {
        Self {
            reply: Some(reply.to_vec()),
            ..Self::new(&[])
        }
    }

    /// До запроса во входном буфере уже лежит `stale` — опоздавший ответ;
    /// ответ на запрос придёт после него.
    pub(crate) fn with_stale(mut self, stale: &[u8]) -> Self {
        self.rx = stale.to_vec();
        self
    }
}

impl ErrorType for Duplex {
    type Error = Infallible;
}

impl Read for Duplex {
    fn read(&mut self, buf: &mut [u8]) -> impl Future<Output = Result<usize, Self::Error>> {
        let n = buf.len().min(self.rx.len() - self.pos);
        buf[..n].copy_from_slice(&self.rx[self.pos..self.pos + n]);
        self.pos += n;
        std::future::ready(Ok(n))
    }
}

impl ReadReady for Duplex {
    fn read_ready(&mut self) -> Result<bool, Self::Error> {
        Ok(self.pos < self.rx.len())
    }
}

impl Write for Duplex {
    fn write(&mut self, buf: &[u8]) -> impl Future<Output = Result<usize, Self::Error>> {
        if let Some(reply) = self.reply.take() {
            self.rx.extend_from_slice(&reply);
        }
        self.tx.extend_from_slice(buf);
        std::future::ready(Ok(buf.len()))
    }

    fn flush(&mut self) -> impl Future<Output = Result<(), Self::Error>> {
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
