//! Modbus TCP: заголовок MBAP и PDU поверх потока байтов.

use embedded_io_async::{Read, ReadExactError, Write};
use ports::{MAX_PDU, MasterError, ModbusClient, ModbusServer, Received, ServeError};

use super::mbap::{self, HEADER, Header, MAX_ADU};
use super::{master_error, serve_error};

/// Modbus TCP поверх потока `io` (сокет `embassy-net`).
///
/// Принятое копится во внутреннем буфере, а не читается `read_exact` в
/// локальный: брошенный на таймауте обмен оставляет недочитанный кадр там,
/// и следующий дочитывает его и пропускает по номеру транзакции — граница
/// кадра в потоке не теряется. Отправка так не защищена: брошенная посреди
/// записи оставляет на линии полкадра, после чего соединение стоит
/// переоткрыть.
///
/// Slave получает любой запрос как адресованный ему, 0xFF (см.
/// [модуль](super)); master ждёт ответ с тем же номером транзакции, что у
/// запроса, и устройство, которое его не повторяет, так и не ответит.
///
/// Соединение — забота потока `io`: закрытое клиентом кончается для slave'а
/// [`ServeError::Eof`], узел перезапускается, и реализация потока обязана
/// на следующем чтении принять новое соединение — как `ports::FrameLink`
/// после `Down`.
#[derive(Debug)]
pub struct Tcp<T> {
    io: T,
    /// Принятые байты текущего кадра — `rx[..have]`.
    rx: [u8; MAX_ADU],
    have: usize,
    /// Номер транзакции: master — последнего запроса, slave — последнего
    /// принятого (им подписан ответ).
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

    async fn fill(&mut self, want: usize) -> Result<(), ReadExactError<T::Error>> {
        super::fill(&mut self.io, &mut self.rx, &mut self.have, want).await
    }

    async fn send(&mut self, tid: u16, unit: u8, pdu: &[u8]) -> Result<(), T::Error> {
        self.io.write_all(&mbap::head(tid, unit, pdu.len())).await?;
        self.io.write_all(pdu).await?;
        self.io.flush().await
    }
}

impl<T: Read + Write> ModbusServer for Tcp<T> {
    type Error = T::Error;

    async fn recv(&mut self, buf: &mut [u8]) -> Result<Received, ServeError<T::Error>> {
        assert!(buf.len() >= MAX_PDU, "буфер PDU {} < {MAX_PDU}", buf.len());
        loop {
            let (header, total) = self.frame().await.map_err(|e| match e {
                Recv::Read(e) => serve_error(e),
                Recv::Framing => ServeError::Framing,
            })?;
            if header.protocol != 0 {
                // Чужой протокол поверх MBAP: спецификация велит его пропустить.
                continue;
            }
            let pdu = &self.rx[HEADER..total];
            buf[..pdu.len()].copy_from_slice(pdu);
            self.tid = header.tid;
            self.unit = header.unit;
            return Ok(Received {
                unit: 0xFF,
                len: pdu.len(),
            });
        }
    }

    async fn reply(&mut self, response: &[u8]) -> Result<(), ServeError<T::Error>> {
        self.send(self.tid, self.unit, response)
            .await
            .map_err(ServeError::Io)
    }
}

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
        self.send(self.tid, unit, request)
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
        self.send(self.tid, 0, request)
            .await
            .map_err(MasterError::Io)
    }
}

#[cfg(test)]
mod tests {
    use core::convert::Infallible;

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
        let mut buf = [0; MAX_PDU];
        // Любой адрес в MBAP — само устройство, и 0 — не широковещание.
        let r = tcp.recv(&mut buf).await.unwrap();
        assert_eq!((r.unit, &buf[..r.len]), (0xFF, &[0x03, 0, 1, 0, 1][..]));
        tcp.reply(&[0x03, 2, 0, 7]).await.unwrap();
        let r = tcp.recv(&mut buf).await.unwrap();
        assert_eq!(r.unit, 0xFF);
        tcp.reply(&[0x06, 0, 0, 0, 1]).await.unwrap();
        let expected = [
            frame(0x0102, 0, 0, &[0x03, 2, 0, 7]),
            frame(9, 0, 5, &[0x06, 0, 0, 0, 1]),
        ]
        .concat();
        assert_eq!(tcp.io.tx, expected);
        assert_eq!(tcp.recv(&mut buf).await, Err(ServeError::Eof));
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
        let mut buf = [0; MAX_PDU];
        assert_eq!(
            tcp.recv(&mut buf)
                .await
                .map(|r| buf[0] == 0x06 && r.len == 5),
            Ok(true)
        );
        assert_eq!(tcp.recv(&mut buf).await, Err(ServeError::Framing));
    }

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

    /// Брошенный посреди приёма обмен не сбивает поток: следующий
    /// дочитывает недочитанный кадр, пропускает его по номеру транзакции и
    /// получает свой.
    #[tokio::test]
    async fn cancelled_exchange_keeps_stream_aligned() {
        let (stale, fresh) = (
            frame(1, 0, 1, &[0x03, 2, 0, 1]),
            frame(2, 0, 1, &[0x03, 2, 0, 2]),
        );
        let mut tcp = Tcp::new(Duplex::replying(&stale[..4]));
        let mut out = [0; MAX_PDU];
        // Ответ на первый запрос обрывается на четвёртом байте: обмен
        // «бросается» на конце потока, как на таймауте.
        assert_eq!(
            tcp.exchange(1, &[0x03, 0, 0, 0, 1], &mut out).await,
            Err(MasterError::Eof)
        );
        tcp.io.push(&[&stale[4..], &fresh[..]].concat());
        assert_eq!(tcp.exchange(1, &[0x03, 0, 0, 0, 1], &mut out).await, Ok(4));
        assert_eq!(out[..4], [0x03, 2, 0, 2]);
    }

    proptest::proptest! {
        /// Что бы ни пришло по соединению — ни slave, ни master не паникуют.
        #[test]
        fn never_panics(input in proptest::collection::vec(proptest::num::u8::ANY, 0..600)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let mut tcp = Tcp::new(Duplex::new(&input));
                let mut buf = [0; MAX_PDU];
                while tcp.recv(&mut buf).await.is_ok() {}
                let _ignored = exchange(1, Duplex::replying(&input)).await;
            });
        }
    }
}
