//! Кадрированный канал поверх потока `embedded_io_async::{Read, Write}`:
//! драйвер sans-IO кодека [`domain::cobs`] — протокол от транспорта отвязан,
//! кодек (COBS, CRC, повторная синхронизация) живёт в `domain` и тестируется
//! без потока, там же.
//!
//! Два буфера. `buf` — буфер канала (`FrameLink::buffer`): в нём копится
//! принятый кадр и декодируется на месте, поэтому на сообщение длиной `n`
//! нужно [`domain::cobs::rx_len`]`(n)` байт — закодированные сообщение и CRC.
//! `frame` — закодированный кадр отправки: на сообщение длиной `n` нужно
//! [`domain::cobs::tx_len`]`(n)`.
//!
//! Разорванное соединение (TCP) поток не восстанавливает: конец потока —
//! [`LinkError::Lost`] с [`CobsError::Eof`], а принять новое соединение — дело
//! потока под ним (`net::Server` делает это на следующем чтении).
//!
//! Хост собирает кадр так же: `b"\0" + cobs_encode(message + crc_le) + b"\0"`.

use domain::cobs::{self, Codec};
use embedded_io_async::{Error, ErrorKind, Read, Write};
use ports::{FrameDuplex, FrameLink, LinkError};

/// Что пошло не так при приёме или отправке кадра.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum CobsError<E> {
    /// Отказ потока.
    Io(E),
    /// Поток кончился: соединение закрыто. Потеря, а не отказ канала: над
    /// TCP-сервером, который принимает следующего клиента (`net::Server`),
    /// следующий `recv` работает. Серий потерь никто не считает: узел на
    /// каждую пишет в лог и сразу зовёт `recv` снова, поэтому поток под
    /// каналом после конца обязан ждать (нового соединения), а не отвечать
    /// концом без ожидания.
    Eof,
    /// Кадр не разобрался как COBS.
    Corrupt,
    /// CRC кадра не сошлась.
    Crc,
    /// Кадр длиннее буфера: при приёме — буфера канала (ему нужно
    /// [`domain::cobs::rx_len`]), при отправке — буфера кадра
    /// ([`domain::cobs::tx_len`]).
    TooLong,
}

/// Кадрированный канал поверх потока `io`.
///
/// Кодек [`Codec`] держит состояние приёма между вызовами `recv`, а не в
/// future: брошенный посреди кадра `recv` не теряет начатый кадр, следующий
/// продолжает с того же байта ([`FrameDuplex`]).
#[derive(Debug)]
pub struct Cobs<'b, T> {
    reader: Reader<T>,
    codec: Codec,
    buf: &'b mut [u8],
    frame: &'b mut [u8],
}

/// Поток и то, что из него прочитано, но ещё не разобрано: `chunk[start..end]`.
///
/// Отдельно от буферов канала: `recv` декодирует в буфер канала и тем
/// временем читает поток, и заимствования двух полей не пересекаются.
#[derive(Debug)]
struct Reader<T> {
    io: T,
    chunk: [u8; CHUNK],
    start: usize,
    end: usize,
}

/// Сколько байт читается из потока за раз. Прочитанное сверх кадра не
/// теряется — ждёт следующего `recv`.
const CHUNK: usize = 64;

impl<'b, T> Cobs<'b, T> {
    /// Канал поверх потока `io` с буфером канала `buf` и буфером кадра
    /// отправки `frame`.
    pub fn new(io: T, buf: &'b mut [u8], frame: &'b mut [u8]) -> Self {
        Self {
            reader: Reader {
                io,
                chunk: [0; CHUNK],
                start: 0,
                end: 0,
            },
            codec: Codec::new(),
            buf,
            frame,
        }
    }

    /// Поток обратно — например, чтобы закрыть сокет.
    pub fn into_inner(self) -> T {
        self.reader.io
    }
}

impl<T: Read> Reader<T> {
    /// Следующий байт потока.
    async fn next_byte(&mut self) -> Result<u8, LinkError<CobsError<T::Error>>> {
        if self.start == self.end {
            let n = self.io.read(&mut self.chunk).await.map_err(io_error)?;
            if n == 0 {
                return Err(LinkError::Lost(CobsError::Eof));
            }
            self.start = 0;
            self.end = n;
        }
        let byte = self.chunk[self.start];
        self.start += 1;
        Ok(byte)
    }
}

impl<T: Read + Write> FrameLink for Cobs<'_, T> {
    type Error = CobsError<T::Error>;

    async fn recv(&mut self) -> Result<usize, LinkError<Self::Error>> {
        let len = loop {
            // Байт уходит из `reader` только в опросе, который его вернул, а
            // кадр копится в кодеке: брошенный здесь `recv` не теряет ничего.
            let byte = match self.reader.next_byte().await {
                Ok(byte) => byte,
                Err(err) => {
                    // Конец потока — новое соединение (`net::Server`), кадр
                    // в нём с начала; отказ — остаток кадра ещё в линии.
                    // Пропуск хвоста, начатый до отказа, тоже не снимается.
                    let midframe = self.codec.is_skipping() || self.codec.is_started();
                    let eof = matches!(err, LinkError::Lost(CobsError::Eof));
                    self.codec.drop_frame(midframe && !eof);
                    return Err(err);
                }
            };
            if let Some(outcome) = self.codec.feed(byte, self.buf) {
                break outcome.map_err(codec_error)?;
            }
        };
        cobs::decode(self.buf, len).map_err(codec_error)
    }

    fn buffer(&mut self) -> &mut [u8] {
        self.buf
    }

    async fn send(&mut self, len: usize) -> Result<(), LinkError<Self::Error>> {
        encode_frame(&mut self.reader.io, self.frame, &self.buf[..len]).await
    }
}

/// Отправка не трогает буфер канала: начатый в нём кадр доживёт до
/// следующего `recv`.
impl<T: Read + Write> FrameDuplex for Cobs<'_, T> {
    async fn send_from(&mut self, message: &[u8]) -> Result<(), LinkError<Self::Error>> {
        encode_frame(&mut self.reader.io, self.frame, message).await
    }
}

/// Кодирует `message` в кадр буфера `frame` и пишет его в поток `io`.
async fn encode_frame<T: Write>(
    io: &mut T,
    frame: &mut [u8],
    message: &[u8],
) -> Result<(), LinkError<CobsError<T::Error>>> {
    let len = cobs::encode(frame, message).map_err(|err| match err {
        cobs::Error::TooLong => LinkError::Lost(CobsError::TooLong),
        // `encode` не возвращает ошибок приёмки: предел проверен первой
        // веткой, остальное — unreachable.
        _ => unreachable!("кодирование не возвращает {err:?}"),
    })?;
    io.write_all(&frame[..len]).await.map_err(io_error)?;
    io.flush().await.map_err(io_error)
}

/// Потеря кадра из кодека — в ошибку канала (отказа транспорта у неё нет).
fn codec_error<E>(err: cobs::Error) -> LinkError<CobsError<E>> {
    LinkError::Lost(match err {
        cobs::Error::Corrupt => CobsError::Corrupt,
        cobs::Error::Crc => CobsError::Crc,
        cobs::Error::TooLong => CobsError::TooLong,
    })
}

/// Отказ потока: разорванное соединение не вылечится следующим кадром, а
/// шум на линии (переполнение, чётность) — вылечится. `embassy-stm32` отдаёт
/// все ошибки линии как `ErrorKind::Other` — значит, «потерян кадр».
fn io_error<E: Error>(e: E) -> LinkError<CobsError<E>> {
    if matches!(
        e.kind(),
        ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionRefused
            | ErrorKind::NotConnected
            | ErrorKind::BrokenPipe
    ) {
        LinkError::Down(CobsError::Io(e))
    } else {
        LinkError::Lost(CobsError::Io(e))
    }
}

#[cfg(test)]
mod tests {
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
    use ports::{FrameDuplex, FrameLink, LinkError};

    use super::{Cobs, CobsError};

    /// Поток: отдаёт `rx` порциями не больше `max` байт — так разыгрываются
    /// разрыв кадра и склейка; пишет в `tx`. На `block_at`-м байте `read`
    /// отвечает `Pending` — так разыгрывается брошенный посреди кадра `recv`.
    struct Stream {
        rx: Vec<u8>,
        pos: usize,
        max: usize,
        block_at: Option<usize>,
        fail: Option<ErrorKind>,
        tx: Vec<u8>,
    }

    impl Stream {
        fn new(rx: Vec<u8>, max: usize) -> Self {
            Self {
                rx,
                pos: 0,
                max,
                block_at: None,
                fail: None,
                tx: Vec::new(),
            }
        }
    }

    impl ErrorType for Stream {
        type Error = ErrorKind;
    }

    impl Read for Stream {
        fn read(&mut self, buf: &mut [u8]) -> impl Future<Output = Result<usize, ErrorKind>> {
            let blocked = self.block_at == Some(self.pos);
            let result = if blocked {
                Ok(0)
            } else if let Some(kind) = self.fail {
                Err(kind)
            } else {
                let mut end = self.rx.len().min(self.pos + self.max.min(buf.len()));
                if let Some(at) = self.block_at.filter(|&at| at > self.pos) {
                    end = end.min(at);
                }
                let n = end - self.pos;
                buf[..n].copy_from_slice(&self.rx[self.pos..end]);
                self.pos = end;
                Ok(n)
            };
            core::future::poll_fn(move |_| {
                if blocked {
                    Poll::Pending
                } else {
                    Poll::Ready(result)
                }
            })
        }
    }

    impl Write for Stream {
        fn write(&mut self, buf: &[u8]) -> impl Future<Output = Result<usize, ErrorKind>> {
            self.tx.extend_from_slice(buf);
            core::future::ready(Ok(buf.len()))
        }

        fn flush(&mut self) -> impl Future<Output = Result<(), ErrorKind>> {
            core::future::ready(Ok(()))
        }
    }

    type Received = Result<Vec<u8>, LinkError<CobsError<ErrorKind>>>;

    /// Кадры сообщений так, как их отправляет канал.
    async fn frames(messages: &[&[u8]]) -> Vec<u8> {
        let (mut buf, mut frame) = ([0; 128], [0; 256]);
        let mut link = Cobs::new(Stream::new(Vec::new(), 1), &mut buf, &mut frame);
        for message in messages {
            link.buffer()[..message.len()].copy_from_slice(message);
            link.send(message.len()).await.unwrap();
        }
        link.into_inner().tx
    }

    /// Всё, что `recv` отдаёт до конца потока.
    async fn recv_all(link: &mut Cobs<'_, Stream>) -> Vec<Received> {
        let mut out = Vec::new();
        loop {
            match link.recv().await {
                Err(LinkError::Lost(CobsError::Eof)) => return out,
                other => out.push(other.map(|len| link.buffer()[..len].to_vec())),
            }
        }
    }

    #[tokio::test]
    async fn split_and_glued_frames_come_out_whole() {
        let wire = frames(&[b"first", b"", b"third message"]).await;
        for max in [1, 3, 64] {
            let (mut buf, mut frame) = ([0; 64], [0; 16]);
            let mut link = Cobs::new(Stream::new(wire.clone(), max), &mut buf, &mut frame);
            let got = recv_all(&mut link).await;
            let want: Vec<Received> = vec![
                Ok(b"first".to_vec()),
                Ok(Vec::new()),
                Ok(b"third message".to_vec()),
            ];
            assert_eq!(got, want, "порции по {max} байт");
        }
    }

    /// Брошенный посреди кадра `recv` не теряет его: следующий `recv`
    /// продолжает с того же байта — сколько бы раз его ни бросали.
    #[tokio::test]
    async fn dropped_recv_resumes_the_same_frame() {
        let first = frames(&[b"a fairly long interrupted frame"]).await;
        let wire = [first.clone(), frames(&[b"whole"]).await].concat();
        let (mut buf, mut frame) = ([0; 64], [0; 16]);
        let mut link = Cobs::new(Stream::new(wire, 64), &mut buf, &mut frame);
        for at in [first.len() / 3, 2 * first.len() / 3] {
            link.reader.io.block_at = Some(at);
            let mut recv = pin!(link.recv());
            let mut cx = Context::from_waker(Waker::noop());
            assert!(recv.as_mut().poll(&mut cx).is_pending());
        }
        link.reader.io.block_at = None;
        let len = link.recv().await.unwrap();
        assert_eq!(&link.buffer()[..len], b"a fairly long interrupted frame");
        let len = link.recv().await.unwrap();
        assert_eq!(&link.buffer()[..len], b"whole");
    }

    /// `send_from` отправляет чужой буфер и не трогает начатый в буфере
    /// канала кадр: брошенный `recv` после отправки дочитывает его целым.
    #[tokio::test]
    async fn send_from_keeps_a_frame_half_received() {
        let first = frames(&[b"half received"]).await;
        let mut stream = Stream::new(first.clone(), 64);
        stream.block_at = Some(first.len() / 2);
        let (mut buf, mut frame) = ([0; 64], [0; 32]);
        let mut link = Cobs::new(stream, &mut buf, &mut frame);
        {
            let mut recv = pin!(link.recv());
            let mut cx = Context::from_waker(Waker::noop());
            assert!(recv.as_mut().poll(&mut cx).is_pending());
        }
        link.send_from(b"reply").await.unwrap();
        link.reader.io.block_at = None;
        let len = link.recv().await.unwrap();
        assert_eq!(&link.buffer()[..len], b"half received");
        assert_eq!(link.into_inner().tx, frames(&[b"reply"]).await);
    }

    #[tokio::test]
    async fn closed_connection_is_down_and_line_noise_is_lost() {
        for (kind, down) in [
            (ErrorKind::ConnectionReset, true),
            (ErrorKind::BrokenPipe, true),
            (ErrorKind::Other, false),
            (ErrorKind::InvalidData, false),
        ] {
            let mut stream = Stream::new(Vec::new(), 64);
            stream.fail = Some(kind);
            let (mut buf, mut frame) = ([0; 64], [0; 16]);
            let mut link = Cobs::new(stream, &mut buf, &mut frame);
            let err = link.recv().await.unwrap_err();
            assert_eq!(
                matches!(err, LinkError::Down(_)),
                down,
                "{kind:?} → {err:?}"
            );
        }
        let (mut buf, mut frame) = ([0; 64], [0; 16]);
        let mut link = Cobs::new(Stream::new(Vec::new(), 64), &mut buf, &mut frame);
        assert_eq!(link.recv().await, Err(LinkError::Lost(CobsError::Eof)));
    }

    #[tokio::test]
    async fn send_refuses_what_does_not_fit_the_tx_buffer() {
        let (mut buf, mut frame) = ([1; 8], [0; 8]);
        let mut link = Cobs::new(Stream::new(Vec::new(), 64), &mut buf, &mut frame);
        assert_eq!(link.send(8).await, Err(LinkError::Lost(CobsError::TooLong)));
        assert!(link.into_inner().tx.is_empty(), "ничего не отправлено");
    }

    /// Кадр длиннее буфера канала — потеря, и следующий кадр принимается.
    #[tokio::test]
    async fn too_long_frame_is_lost_and_the_next_is_received() {
        let wire = frames(&[&[7; 100], b"next"]).await;
        let (mut buf, mut frame) = ([0; 64], [0; 16]);
        let mut link = Cobs::new(Stream::new(wire, 64), &mut buf, &mut frame);
        let got = recv_all(&mut link).await;
        let want: Vec<Received> = vec![Err(LinkError::Lost(CobsError::TooLong)), Ok(b"next".to_vec())];
        assert_eq!(got, want);
    }
}
