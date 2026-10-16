//! Транспорты Modbus: RTU поверх потока байтов, TCP поверх потока, UDP
//! поверх датаграмм. Каждый реализует оба порта — [`ports::ModbusClient`]
//! для master'а и [`ports::ModbusServer`] для slave'а, — и логика
//! `domain::modbus` работает с PDU, не зная, какой транспорт под ней.
//!
//! Чем транспорты отличаются, и почему каждый устроен так:
//!
//! * [`Rtu`](crate::modbus::Rtu) — кадр «адрес, PDU, CRC» поверх
//!   `embedded_io_async::{Read, Write}`. Пауза между кадрами в потоке не
//!   видна, поэтому граница кадра выводится из содержимого: длина — из
//!   кода функции и счётчика байт, CRC подтверждает, что кадр найден верно;
//!   после битого кадра slave сдвигается на байт, пока CRC не сойдётся
//!   снова.
//! * [`Tcp`](crate::modbus::Tcp) — заголовок MBAP (номер транзакции,
//!   протокол 0, длина, адрес) и PDU поверх потока. CRC нет — целостность
//!   обеспечивает TCP, границу — длина в заголовке. Длина вне допустимой —
//!   граница потеряна, и поток не выровнять: slave кончается
//!   [`ServeError::Framing`](ports::ServeError::Framing), master —
//!   [`MasterError::Unexpected`](ports::MasterError::Unexpected).
//! * [`Udp`](crate::modbus::Udp) — тот же MBAP, по запросу или ответу в
//!   датаграмме (`ports::FrameLink`): границу даёт датаграмма, и битая
//!   просто пропускается.
//!
//! Номер транзакции master'а растёт с каждым запросом, ответ с чужим
//! номером — опоздавший ответ на брошенный по таймауту запрос — пропускается.
//! В RTU номера нет, и там то же делает слив входного буфера перед запросом.
//!
//! TCP и UDP адресуют устройство по IP, и адрес в MBAP нужен только шлюзу
//! на шину RS-485. Поэтому slave получает любой запрос по TCP и UDP как
//! адресованный ему (0xFF), а ответ несёт адрес из запроса. Master
//! по-прежнему шлёт адресу 0 без ожидания ответа
//! ([`ports::ModbusClient::broadcast`]): ответ, если он придёт, будет
//! пропущен по номеру транзакции; к самому устройству за TCP master
//! обращается адресом 0xFF.

/// Заголовок MBAP — общий для TCP и UDP.
mod mbap;

/// Modbus RTU: кадр с CRC поверх потока байтов.
mod rtu;

/// Modbus TCP: MBAP поверх потока байтов.
mod tcp;

/// Modbus UDP: MBAP в датаграмме.
mod udp;

pub use rtu::Rtu;
pub use tcp::Tcp;
pub use udp::Udp;

use embedded_io_async::{Read, ReadExactError};
use ports::{MasterError, ServeError};

/// Дочитывает `rx` из `io` до `want` байт. Каждый прочитанный кусок сразу
/// учтён в `have`, поэтому брошенное чтение ничего не теряет: следующий
/// вызов продолжит с того же места.
async fn fill<T: Read>(
    io: &mut T,
    rx: &mut [u8],
    have: &mut usize,
    want: usize,
) -> Result<(), ReadExactError<T::Error>> {
    while *have < want {
        match io.read(&mut rx[*have..want]).await {
            Ok(0) => return Err(ReadExactError::UnexpectedEof),
            Ok(n) => *have += n,
            Err(e) => return Err(ReadExactError::Other(e)),
        }
    }
    Ok(())
}

fn serve_error<E>(e: ReadExactError<E>) -> ServeError<E> {
    match e {
        ReadExactError::UnexpectedEof => ServeError::Eof,
        ReadExactError::Other(e) => ServeError::Io(e),
    }
}

fn master_error<E>(e: ReadExactError<E>) -> MasterError<E> {
    match e {
        ReadExactError::UnexpectedEof => MasterError::Eof,
        ReadExactError::Other(e) => MasterError::Io(e),
    }
}

#[cfg(test)]
mod fakes {
    //! Фейки для host-тестов транспортов: поток байтов и канал датаграмм.
    //!
    //! Ни один не ждёт по-настоящему — их future готовы с первого `poll`, —
    //! поэтому тестам хватает однопоточного `#[tokio::test]`.

    use core::convert::Infallible;
    use std::collections::VecDeque;

    use embedded_io_async::{ErrorType, Read, ReadReady, Write};
    use ports::{FrameLink, LinkError};

    use super::mbap;
    use super::rtu::seal;

    /// Кадр RTU: адрес `unit`, `pdu` и CRC.
    pub(crate) fn rtu_frame(unit: u8, pdu: &[u8]) -> Vec<u8> {
        let mut buf = [&[unit][..], pdu, &[0, 0]].concat();
        let len = buf.len() - 2;
        seal(&mut buf, len);
        buf
    }

    /// Кадр MBAP: транзакция `tid`, протокол `protocol`, адрес `unit`, `pdu`.
    pub(crate) fn mbap_frame(tid: u16, protocol: u16, unit: u8, pdu: &[u8]) -> Vec<u8> {
        let mut head = mbap::head(tid, unit, pdu.len());
        head[2..4].copy_from_slice(&protocol.to_be_bytes());
        [&head[..], pdu].concat()
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
        /// Когда читать нечего — ждать (`Pending`), а не отдавать конец потока:
        /// так в тесте бросается future посреди приёма, как на таймауте.
        waits: bool,
        pub(crate) tx: Vec<u8>,
    }

    impl Duplex {
        /// Байты читаемы сразу — вход slave'а.
        pub(crate) fn new(rx: &[u8]) -> Self {
            Self {
                rx: rx.to_vec(),
                pos: 0,
                reply: None,
                waits: false,
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

        /// Пустой вход — ожидание, а не конец потока.
        pub(crate) fn waiting(mut self) -> Self {
            self.waits = true;
            self
        }

        /// Дописывает `bytes` ко входу — пришли позже.
        pub(crate) fn push(&mut self, bytes: &[u8]) {
            self.rx.extend_from_slice(bytes);
        }
    }

    impl ErrorType for Duplex {
        type Error = Infallible;
    }

    impl Read for Duplex {
        fn read(&mut self, buf: &mut [u8]) -> impl Future<Output = Result<usize, Self::Error>> {
            std::future::poll_fn(move |_| {
                let n = buf.len().min(self.rx.len() - self.pos);
                if n == 0 && self.waits && !buf.is_empty() {
                    return std::task::Poll::Pending;
                }
                buf[..n].copy_from_slice(&self.rx[self.pos..self.pos + n]);
                self.pos += n;
                std::task::Poll::Ready(Ok(n))
            })
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

    /// Канал датаграмм: отдаёт заранее заданные исходы приёма, затем — `Down`;
    /// отправленное копит в `sent`.
    pub(crate) struct Datagrams {
        pub(crate) incoming: VecDeque<Result<Vec<u8>, LinkError<&'static str>>>,
        /// Исход каждой отправки; успешные попадают в `sent`.
        pub(crate) send: Result<(), LinkError<&'static str>>,
        pub(crate) sent: Vec<Vec<u8>>,
    }

    impl Datagrams {
        pub(crate) fn with(
            incoming: impl IntoIterator<Item = Result<Vec<u8>, LinkError<&'static str>>>,
        ) -> Self {
            Self {
                incoming: incoming.into_iter().collect(),
                send: Ok(()),
                sent: Vec::new(),
            }
        }
    }

    impl FrameLink for Datagrams {
        type Error = &'static str;

        fn recv(
            &mut self,
            buf: &mut [u8],
        ) -> impl Future<Output = Result<usize, LinkError<Self::Error>>> {
            let next = match self.incoming.pop_front() {
                Some(Ok(datagram)) if datagram.len() > buf.len() => {
                    Err(LinkError::Lost("длиннее буфера"))
                }
                Some(Ok(datagram)) => {
                    buf[..datagram.len()].copy_from_slice(&datagram);
                    Ok(datagram.len())
                }
                Some(Err(e)) => Err(e),
                None => Err(LinkError::Down("конец")),
            };
            std::future::ready(next)
        }

        fn send(
            &mut self,
            frame: &[u8],
        ) -> impl Future<Output = Result<(), LinkError<Self::Error>>> {
            if self.send.is_ok() {
                self.sent.push(frame.to_vec());
            }
            std::future::ready(self.send)
        }
    }
}
