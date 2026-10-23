//! Транспорт Modbus {% if modbus_rtu == "true" %}— RTU поверх потока байтов{% elsif modbus_tcp == "true" %}— TCP поверх потока{% else %}— UDP поверх датаграмм{% endif %}.
//! Для slave'а он — кадровый канал [`ports::FrameLink`] с кадром «адрес и PDU»:
//! кадр разбирает обработчик `domain::modbus::Slave`, а обслуживает канал узел
//! `domain::server`, тот же, что у protobuf.
// {%- if modbus_master == "true" %}
//! Они же реализуют порт master'а [`ports::ModbusClient`].
// {%- endif %}
//! Логика `domain::modbus` работает с PDU, не зная, какой транспорт под ней.
//!
// {%- if modbus_rtu == "true" %}
//! [`Rtu`](crate::modbus::Rtu) — кадр «адрес, PDU, CRC» поверх
//! `embedded_io_async::{Read, Write}`. Пауза между кадрами в потоке не
//! видна, поэтому граница кадра выводится из содержимого: длина — из
//! кода функции и счётчика байт, CRC подтверждает, что кадр найден верно;
//! после битого кадра slave сдвигается на байт, пока CRC не сойдётся
//! снова.
//!
// {%- endif %}
// {%- if modbus_tcp == "true" %}
//! [`Tcp`](crate::modbus::Tcp) — заголовок MBAP (номер транзакции,
//! протокол 0, длина, адрес) и PDU поверх потока. CRC нет — целостность
//! обеспечивает TCP, границу — длина в заголовке. Длина вне допустимой —
//! граница потеряна, и поток не выровнять: канал отказывает
//! ([`LinkError::Down`](ports::LinkError::Down) с [`FrameError::Framing`](crate::modbus::FrameError::Framing)).
// {%- if modbus_master == "true" %}
//! Master в этом случае получает
//! [`MasterError::Unexpected`](ports::MasterError::Unexpected).
// {%- endif %}
//!
// {%- endif %}
// {%- if modbus_udp == "true" %}
//! [`Udp`](crate::modbus::Udp) — заголовок MBAP, по запросу или ответу в
//! датаграмме (`ports::FrameLink`): границу даёт датаграмма, и битая
//! просто пропускается.
//!
// {%- endif %}
// {%- if modbus_master == "true" %}
// {%- if modbus_mbap == "true" %}
//! Номер транзакции master'а растёт с каждым запросом, ответ с чужим
//! номером — опоздавший ответ на брошенный по таймауту запрос — пропускается.
// {%- endif %}
// {%- if modbus_rtu == "true" %}
//! В RTU номера нет, и там то же делает слив входного буфера перед запросом.
//!
// {%- endif %}
// {%- endif %}
// {%- if modbus_mbap == "true" %}
//! TCP и UDP адресуют устройство по IP, и адрес в MBAP нужен только шлюзу
//! на шину RS-485. Поэтому slave получает любой запрос как адресованный
//! ему (0xFF), а ответ несёт адрес из запроса.
// {%- if modbus_master == "true" %}
//! Master по-прежнему шлёт адресу 0 без ожидания ответа
//! ([`ports::ModbusClient::broadcast`]): ответ, если он придёт, будет
//! пропущен по номеру транзакции; к самому устройству master
//! обращается адресом 0xFF.
// {%- endif %}
// {%- endif %}

// {%- if modbus_mbap == "true" %}
/// Заголовок MBAP — общий для TCP и UDP.
mod mbap;

// {%- endif %}
// {%- if modbus_rtu == "true" %}
/// Modbus RTU: кадр с CRC поверх потока байтов.
mod rtu;

// {%- endif %}
// {%- if modbus_tcp == "true" %}
/// Modbus TCP: MBAP поверх потока байтов.
mod tcp;

// {%- endif %}
// {%- if modbus_udp == "true" %}
/// Modbus UDP: MBAP в датаграмме.
mod udp;
// {%- endif %}

// {%- if modbus_rtu == "true" %}
pub use rtu::Rtu;
// {%- endif %}
// {%- if modbus_tcp == "true" %}
pub use tcp::Tcp;
// {%- endif %}
// {%- if modbus_udp == "true" %}
pub use udp::{DATAGRAM, Udp};
// {%- endif %}

// {%- if modbus_stream == "true" %}
use embedded_io_async::{Error, ErrorKind, Read, ReadExactError};
use ports::LinkError;
// {%- endif %}

// {%- if modbus_stream == "true" %}
/// Почему поток Modbus (RTU, TCP) не принял или не отправил кадр.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum FrameError<E> {
    /// Отказ потока.
    Io(E),
    /// Поток кончился: соединение закрыто. Потеря, а не отказ канала: над
    /// TCP-сервером (`net::TcpServer`) следующий `recv` примет нового клиента.
    Eof,
    /// Длина в заголовке MBAP вне допустимой: граница кадра в потоке потеряна,
    /// и соединение стоит переоткрыть.
    Framing,
}

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

/// Отказ потока как исход канала: разорванное соединение — отказ, шум на
/// UART (переполнение, чётность; `embassy-stm32` отдаёт их как
/// `ErrorKind::Other`) — потеря кадра.
fn io_error<E: Error>(e: E) -> LinkError<FrameError<E>> {
    if matches!(
        e.kind(),
        ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionRefused
            | ErrorKind::NotConnected
            | ErrorKind::BrokenPipe
    ) {
        LinkError::Down(FrameError::Io(e))
    } else {
        LinkError::Lost(FrameError::Io(e))
    }
}

fn read_error<E: Error>(e: ReadExactError<E>) -> LinkError<FrameError<E>> {
    match e {
        ReadExactError::UnexpectedEof => LinkError::Lost(FrameError::Eof),
        ReadExactError::Other(e) => io_error(e),
    }
}

// {%- if modbus_master == "true" %}
fn master_error<E>(e: ReadExactError<E>) -> ports::MasterError<E> {
    match e {
        ReadExactError::UnexpectedEof => ports::MasterError::Eof,
        ReadExactError::Other(e) => ports::MasterError::Io(e),
    }
}
// {%- endif %}
// {%- endif %}

#[cfg(test)]
mod fakes {
    //! Фейки для host-тестов транспортов: поток байтов и канал датаграмм.
    //!
    //! Ни один не ждёт по-настоящему — их future готовы с первого `poll`, —
    //! поэтому тестам хватает однопоточного `#[tokio::test]`.

    // {%- if modbus_stream == "true" %}
    use core::convert::Infallible;
    // {%- endif %}
    // {%- if modbus_udp == "true" %}
    use std::collections::VecDeque;
    // {%- endif %}

    // {%- if modbus_stream == "true" %}
    use embedded_io_async::{ErrorType, Read, ReadReady, Write};
    // {%- endif %}
    // {%- if modbus_udp == "true" %}
    use ports::{FrameLink, LinkError};
    // {%- endif %}

    // {%- if modbus_mbap == "true" %}
    use super::mbap;
    // {%- endif %}
    // {%- if modbus_rtu == "true" %}
    use super::rtu::seal;
    // {%- endif %}

    // {%- if modbus_stream == "true" %}
    /// Поток: читает заранее записанные байты, пишет в `tx`. Когда читать
    /// нечего, `read` отдаёт 0 — конец потока.
    pub(super) struct Duplex {
        rx: Vec<u8>,
        /// Сколько из `rx` уже прочитано.
        pos: usize,
        /// Ответ, который дописывается к входу только после первой записи — как
        /// на линии, где slave отвечает на запрос, а не до него.
        reply: Option<Vec<u8>>,
        /// Когда читать нечего — ждать (`Pending`), а не отдавать конец потока:
        /// так в тесте бросается future посреди приёма, как на таймауте.
        waits: bool,
        pub(super) tx: Vec<u8>,
    }

    impl Duplex {
        /// Байты читаемы сразу — вход slave'а.
        pub(super) fn new(rx: &[u8]) -> Self {
            Self {
                rx: rx.to_vec(),
                pos: 0,
                reply: None,
                waits: false,
                tx: Vec::new(),
            }
        }

        // {%- if modbus_master == "true" %}
        /// `reply` читаем только после того, как master отправил запрос.
        pub(super) fn replying(reply: &[u8]) -> Self {
            Self {
                reply: Some(reply.to_vec()),
                ..Self::new(&[])
            }
        }

        // {%- if modbus_rtu == "true" %}
        /// До запроса во входном буфере уже лежит `stale` — опоздавший ответ;
        /// ответ на запрос придёт после него.
        pub(super) fn with_stale(mut self, stale: &[u8]) -> Self {
            self.rx = stale.to_vec();
            self
        }
        // {%- endif %}
        // {%- if modbus_tcp == "true" %}

        /// Пустой вход — ожидание, а не конец потока.
        pub(super) fn waiting(mut self) -> Self {
            self.waits = true;
            self
        }
        // {%- endif %}
        // {%- endif %}
        // {%- if modbus_tcp == "true" %}

        /// Дописывает `bytes` ко входу — пришли позже.
        pub(super) fn push(&mut self, bytes: &[u8]) {
            self.rx.extend_from_slice(bytes);
        }
        // {%- endif %}
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

    // {%- endif %}

    // {%- if modbus_udp == "true" %}
    /// Канал датаграмм: отдаёт заранее заданные исходы приёма, затем — `Down`;
    /// отправленное копит в `sent`.
    pub(super) struct Datagrams {
        pub(super) incoming: VecDeque<Result<Vec<u8>, LinkError<&'static str>>>,
        /// Исход каждой отправки; успешные попадают в `sent`.
        pub(super) send: Result<(), LinkError<&'static str>>,
        pub(super) sent: Vec<Vec<u8>>,
        /// Буфер канала — на самую длинную датаграмму Modbus.
        buffer: [u8; super::mbap::MAX_ADU],
    }

    impl Datagrams {
        pub(super) fn with(
            incoming: impl IntoIterator<Item = Result<Vec<u8>, LinkError<&'static str>>>,
        ) -> Self {
            Self {
                incoming: incoming.into_iter().collect(),
                send: Ok(()),
                sent: Vec::new(),
                buffer: [0; super::mbap::MAX_ADU],
            }
        }
    }

    impl FrameLink for Datagrams {
        type Error = &'static str;

        fn recv(&mut self) -> impl Future<Output = Result<usize, LinkError<Self::Error>>> {
            let buf = &mut self.buffer;
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

        fn buffer(&mut self) -> &mut [u8] {
            &mut self.buffer
        }

        fn send(&mut self, len: usize) -> impl Future<Output = Result<(), LinkError<Self::Error>>> {
            if self.send.is_ok() {
                self.sent.push(self.buffer[..len].to_vec());
            }
            std::future::ready(self.send)
        }
    }
    // {%- endif %}

    // {%- if modbus_rtu == "true" %}
    /// Кадр RTU: адрес `unit`, `pdu` и CRC.
    pub(super) fn rtu_frame(unit: u8, pdu: &[u8]) -> Vec<u8> {
        let mut buf = [&[unit][..], pdu, &[0, 0]].concat();
        let len = buf.len() - 2;
        seal(&mut buf, len);
        buf
    }
    // {%- endif %}

    // {%- if modbus_mbap == "true" %}
    /// Кадр MBAP: транзакция `tid`, протокол `protocol`, адрес `unit`, `pdu`.
    pub(super) fn mbap_frame(tid: u16, protocol: u16, unit: u8, pdu: &[u8]) -> Vec<u8> {
        let mut head = mbap::head(tid, unit, pdu.len());
        head[2..4].copy_from_slice(&protocol.to_be_bytes());
        [&head[..], pdu].concat()
    }
    // {%- endif %}
}
