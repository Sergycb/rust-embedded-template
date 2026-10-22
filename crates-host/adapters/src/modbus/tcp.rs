//! Modbus TCP: заголовок MBAP и PDU поверх потока байтов.

use embedded_io_async::{Read, ReadExactError, Write};
use ports::{Assemble, FrameLink, LinkError};
// {%- if modbus_master == "true" %}
use ports::MAX_PDU;
// {%- endif %}

// {%- if modbus_master == "true" %}
use ports::{MasterError, ModbusClient};

// {%- endif %}
use super::mbap::{self, HEADER, Header, MAX_ADU};
// {%- if modbus_master == "true" %}
use super::master_error;
// {%- endif %}
use super::{FrameError, io_error, read_error};

/// Modbus TCP поверх потока `io` (сокет `embassy-net`).
///
/// Принятое копится во внутреннем буфере, а не читается `read_exact` в
/// локальный: брошенный на таймауте обмен оставляет недочитанный кадр там,
/// и следующий дочитывает его и пропускает по номеру транзакции — граница
/// кадра в потоке не теряется. Отправка так не защищена: брошенная посреди
/// записи оставляет на линии полкадра, после чего соединение стоит
/// переоткрыть.
///
/// Для slave'а — канал [`FrameLink`] с кадром «адрес и PDU» прямо в приёмном
/// буфере, за заголовком: любой запрос приходит как адресованный самому
/// устройству, 0xFF (см. [модуль](super)), а ответ несёт адрес и номер
/// транзакции запроса.
// {%- if modbus_master == "true" %}
/// Master ждёт ответ с тем же номером транзакции, что у запроса, и
/// устройство, которое его не повторяет, так и не ответит.
// {%- endif %}
///
/// Соединение — забота потока `io`: закрытое клиентом — потеря кадра
/// ([`FrameError::Eof`]), а не отказ: узел обслуживает дальше, и реализация
/// потока обязана на следующем чтении принять новое соединение.
#[derive(Debug)]
pub struct Tcp<T> {
    io: T,
    /// Принятые байты текущего кадра — `rx[..have]`; для slave'а с
    /// адреса (`rx[HEADER - 1..]`) это и буфер канала.
    rx: [u8; MAX_ADU],
    have: usize,
    /// Номер транзакции последнего принятого запроса — им подписан ответ.
    // {%- if modbus_master == "true" %}
    /// У master — последнего отправленного: по нему ищется ответ.
    // {%- endif %}
    tid: u16,
    /// Адрес из последнего принятого запроса — slave повторяет его в ответе.
    unit: u8,
}

impl<T> Tcp<T> {
    /// Транспорт поверх потока `io`.
    pub const fn new(io: T) -> Self {
        Self {
            io,
            rx: [0; MAX_ADU],
            have: 0,
            tid: 0,
            unit: 0,
        }
    }
}

/// Почему не принят кадр.
enum Recv<E> {
    Read(ReadExactError<E>),
    /// Длина в заголовке вне допустимой.
    Framing,
}

impl<E> From<ReadExactError<E>> for Recv<E> {
    fn from(e: ReadExactError<E>) -> Self {
        Self::Read(e)
    }
}

impl<T: Read + Write> Tcp<T> {
    /// Принимает кадр целиком в начало `rx`; возвращает заголовок и длину
    /// кадра.
    async fn frame(&mut self) -> Result<(Header, usize), Recv<T::Error>> {
        self.fill(HEADER).await?;
        let header = Header::parse(&self.rx);
        let Some(total) = header.total() else {
            self.have = 0;
            return Err(Recv::Framing);
        };
        self.fill(total).await?;
        self.have = 0;
        Ok((header, total))
    }

    /// Брошенное чтение (таймаут) недочитанный кадр сохраняет — следующий
    /// обмен его дочитает. Оборванное (конец потока, ошибка) — сбрасывает:
    /// поток над TCP-сервером продолжится уже другим соединением, и хвост
    /// прошлого склеился бы с его первым кадром.
    async fn fill(&mut self, want: usize) -> Result<(), ReadExactError<T::Error>> {
        let result = super::fill(&mut self.io, &mut self.rx, &mut self.have, want).await;
        if result.is_err() {
            self.have = 0;
        }
        result
    }

    // {%- if modbus_master == "true" %}
    async fn send_pdu(&mut self, tid: u16, unit: u8, pdu: &[u8]) -> Result<(), T::Error> {
        self.io.write_all(&mbap::head(tid, unit, pdu.len())).await?;
        self.io.write_all(pdu).await?;
        self.io.flush().await
    }
    // {%- endif %}
}

/// Собирается вокруг собранного потока: `Tcp<TcpServer>` — над стеком платы.
impl<T: Assemble> Assemble for Tcp<T> {
    type Source = T::Source;
    type Buffers = T::Buffers;

    fn assemble(source: T::Source, buffers: T::Buffers) -> Self {
        Self::new(T::assemble(source, buffers))
    }
}

impl<T: Read + Write> FrameLink for Tcp<T> {
    type Error = FrameError<T::Error>;

    async fn recv(&mut self) -> Result<usize, LinkError<Self::Error>> {
        loop {
            let (header, total) = self.frame().await.map_err(|e| match e {
                Recv::Read(e) => read_error(e),
                Recv::Framing => LinkError::Down(FrameError::Framing),
            })?;
            if header.protocol != 0 {
                // Чужой протокол поверх MBAP: спецификация велит его пропустить.
                continue;
            }
            self.tid = header.tid;
            self.unit = header.unit;
            self.rx[HEADER - 1] = 0xFF;
            return Ok(total - (HEADER - 1));
        }
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.rx[HEADER - 1..]
    }

    /// Ответ — с номером транзакции и адресом запроса: заголовок пишется
    /// перед кадром в том же буфере.
    async fn send(&mut self, len: usize) -> Result<(), LinkError<Self::Error>> {
        let pdu = len.checked_sub(1).expect("кадр ответа — адрес и PDU");
        self.rx[..HEADER].copy_from_slice(&mbap::head(self.tid, self.unit, pdu));
        self.io
            .write_all(&self.rx[..HEADER + pdu])
            .await
            .map_err(io_error)?;
        self.io.flush().await.map_err(io_error)
    }
}

// {%- if modbus_master == "true" %}
impl<T: Read + Write> ModbusClient for Tcp<T> {
    type Error = T::Error;

    async fn exchange(
        &mut self,
        unit: u8,
        request: &[u8],
        response: &mut [u8],
    ) -> Result<usize, MasterError<T::Error>> {
        assert!(
            response.len() >= MAX_PDU,
            "буфер PDU {} < {MAX_PDU}",
            response.len()
        );
        self.tid = self.tid.wrapping_add(1);
        self.send_pdu(self.tid, unit, request)
            .await
            .map_err(MasterError::Io)?;
        loop {
            let (header, total) = self.frame().await.map_err(|e| match e {
                Recv::Read(e) => master_error(e),
                Recv::Framing => MasterError::Unexpected,
            })?;
            if header.protocol != 0 || header.tid != self.tid {
                // Опоздавший ответ на брошенный запрос или чужой протокол.
                continue;
            }
            if header.unit != unit {
                return Err(MasterError::Unexpected);
            }
            let pdu = &self.rx[HEADER..total];
            response[..pdu.len()].copy_from_slice(pdu);
            return Ok(pdu.len());
        }
    }

    async fn broadcast(&mut self, request: &[u8]) -> Result<(), MasterError<T::Error>> {
        self.tid = self.tid.wrapping_add(1);
        self.send_pdu(self.tid, 0, request)
            .await
            .map_err(MasterError::Io)
    }
}
// {%- endif %}

#[cfg(test)]
mod tests {
    // {%- if modbus_master == "true" %}
    use core::convert::Infallible;

    // {%- endif %}
    use super::*;
    use crate::modbus::fakes::{Duplex, mbap_frame as frame};

    #[tokio::test]
    async fn server_echoes_transaction_and_unit() {
        let input = [
            frame(0x0102, 0, 0, &[0x03, 0, 1, 0, 1]),
            frame(9, 0, 5, &[0x06, 0, 0, 0, 1]),
        ]
        .concat();
        let mut tcp = Tcp::new(Duplex::new(&input));
        // Любой адрес в MBAP — само устройство, и 0 — не широковещание.
        let len = tcp.recv().await.unwrap();
        assert_eq!(&tcp.buffer()[..len], &[0xFF, 0x03, 0, 1, 0, 1]);
        tcp.buffer()[1..5].copy_from_slice(&[0x03, 2, 0, 7]);
        tcp.send(5).await.unwrap();
        let len = tcp.recv().await.unwrap();
        assert_eq!(tcp.buffer()[0], 0xFF);
        tcp.send(len).await.unwrap();
        let expected = [
            frame(0x0102, 0, 0, &[0x03, 2, 0, 7]),
            frame(9, 0, 5, &[0x06, 0, 0, 0, 1]),
        ]
        .concat();
        assert_eq!(tcp.io.tx, expected);
        assert_eq!(tcp.recv().await, Err(LinkError::Lost(FrameError::Eof)));
    }

    #[tokio::test]
    async fn server_skips_foreign_protocol_and_loses_bad_length() {
        let mut bad_length = frame(1, 0, 1, &[0x03]);
        bad_length[5] = 0;
        let input = [
            frame(1, 7, 1, &[0x03, 0, 0, 0, 1]),
            frame(2, 0, 1, &[0x06, 0, 0, 0, 1]),
            bad_length,
        ]
        .concat();
        let mut tcp = Tcp::new(Duplex::new(&input));
        assert_eq!(tcp.recv().await, Ok(6));
        assert_eq!(tcp.buffer()[1], 0x06);
        assert_eq!(tcp.recv().await, Err(LinkError::Down(FrameError::Framing)));
    }

    // {%- if modbus_master == "true" %}
    async fn exchange(unit: u8, io: Duplex) -> (Result<Vec<u8>, MasterError<Infallible>>, Vec<u8>) {
        let mut tcp = Tcp::new(io);
        let mut out = [0; MAX_PDU];
        let r = tcp.exchange(unit, &[0x03, 0, 0, 0, 1], &mut out).await;
        (r.map(|len| out[..len].to_vec()), tcp.io.tx)
    }

    #[tokio::test]
    async fn client_skips_stale_transactions() {
        let reply = [
            // опоздавший ответ на прошлый запрос
            frame(0, 0, 1, &[0x03, 2, 0, 1]),
            frame(1, 0, 1, &[0x03, 2, 0, 2]),
        ]
        .concat();
        let (got, tx) = exchange(1, Duplex::replying(&reply)).await;
        assert_eq!(got, Ok(vec![0x03, 2, 0, 2]));
        assert_eq!(tx, frame(1, 0, 1, &[0x03, 0, 0, 0, 1]));
    }

    #[tokio::test]
    async fn client_rejects_broken_replies() {
        let mut bad_length = frame(1, 0, 1, &[0x03]);
        bad_length[5] = 0xFF;
        let cases = [
            (frame(1, 0, 2, &[0x03, 2, 0, 1]), MasterError::Unexpected),
            (bad_length, MasterError::Unexpected),
            (
                frame(1, 0, 1, &[0x03, 2, 0, 1])[..9].to_vec(),
                MasterError::Eof,
            ),
        ];
        for (reply, error) in cases {
            let (got, _) = exchange(1, Duplex::replying(&reply)).await;
            assert_eq!(got, Err(error), "ответ {reply:02X?}");
        }
    }

    /// Брошенный посреди приёма обмен (таймаут) не сбивает поток:
    /// следующий дочитывает недочитанный кадр, пропускает его по номеру
    /// транзакции и получает свой.
    #[tokio::test]
    async fn cancelled_exchange_keeps_stream_aligned() {
        let (stale, fresh) = (
            frame(1, 0, 1, &[0x03, 2, 0, 1]),
            frame(2, 0, 1, &[0x03, 2, 0, 2]),
        );
        let mut tcp = Tcp::new(Duplex::replying(&stale[..4]).waiting());
        let mut out = [0; MAX_PDU];
        {
            // Ответ на первый запрос пришёл до четвёртого байта, дальше —
            // тишина: один опрос, и future бросается, как на таймауте.
            let exchange = std::pin::pin!(tcp.exchange(1, &[0x03, 0, 0, 0, 1], &mut out));
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(exchange.poll(&mut cx).is_pending());
        }
        tcp.io.push(&[&stale[4..], &fresh[..]].concat());
        assert_eq!(tcp.exchange(1, &[0x03, 0, 0, 0, 1], &mut out).await, Ok(4));
        assert_eq!(out[..4], [0x03, 2, 0, 2]);
    }
    // {%- endif %}

    /// Оборванное чтение (конец потока) — не таймаут: недочитанный кадр
    /// сбрасывается, и поток, продолжившийся другим соединением
    /// (`net::TcpServer`), читается с начала кадра.
    #[tokio::test]
    async fn ended_stream_drops_the_partial_frame() {
        let fresh = frame(1, 0, 7, &[0x03, 0, 1, 0, 1]);
        let mut tcp = Tcp::new(Duplex::new(&fresh[..4]));
        assert_eq!(tcp.recv().await, Err(LinkError::Lost(FrameError::Eof)));
        tcp.io.push(&fresh);
        let len = tcp.recv().await.unwrap();
        assert_eq!(&tcp.buffer()[1..len], &[0x03, 0, 1, 0, 1]);
    }

    proptest::proptest! {
        /// Что бы ни пришло по соединению — транспорт не паникует.
        #[test]
        fn never_panics(input in proptest::collection::vec(proptest::num::u8::ANY, 0..600)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let mut tcp = Tcp::new(Duplex::new(&input));
                while tcp.recv().await.is_ok() {}
                // {%- if modbus_master == "true" %}
                let _ignored = exchange(1, Duplex::replying(&input)).await;
                // {%- endif %}
            });
        }
    }
}
