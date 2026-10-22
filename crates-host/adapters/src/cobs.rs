//! Кадрирование потока байтов: [`Cobs`](crate::cobs::Cobs) реализует
//! [`FrameLink`](ports::FrameLink) поверх любого
//! `embedded_io_async::{Read, Write}` — UART, USB CDC, TCP-сокета.
//!
//! Кадр на проводе: `0x00`, COBS от `[сообщение][CRC]`, `0x00`.
//!
//! * **COBS** переписывает данные так, что в них нет `0x00`, поэтому `0x00`
//!   однозначно отмечает границу кадра. Приёмник, потерявший синхронизацию
//!   (помеха, включился посреди передачи, брошенный `recv`), ждёт следующий
//!   `0x00` — и с него снова читает кадры. Накладные расходы — байт на 254.
//! * **`0x00` и в начале кадра**: если отправку бросили на середине (таймаут
//!   у вызывающего), обрывок без конца не склеится со следующим кадром — его
//!   отрежет ведущий `0x00` следующего. Пустые кадры между `0x00` приёмник
//!   пропускает.
//! * **CRC-16/IBM-SDLC** (X.25, FCS у HDLC и PPP) — младшим байтом первым —
//!   отсекает искажённый и обрезанный кадр: без неё protobuf разобрал бы
//!   его молча, с неверными полями.
//!
//! Хост собирает кадр так же: `b"\0" + cobs_encode(message + crc_le) + b"\0"`.

use ::cobs::{CobsDecoder, CobsEncoder, DecodeError, max_encoding_length};
use crc::{CRC_16_IBM_SDLC, Crc, NoTable};
use embedded_io_async::{Error, ErrorKind, Read, Write};
use ports::{Assemble, FrameLink, LinkError};

/// CRC кадра. Без таблицы: 512 байт флеша не стоят выигрыша в скорости на
/// кадрах в сотни байт.
const CRC: Crc<u16, NoTable> = Crc::<u16, NoTable>::new(&CRC_16_IBM_SDLC);

/// Длина CRC в кадре.
const CRC_LEN: usize = 2;

/// Сколько байт читается из потока за раз. Прочитанное сверх кадра не
/// теряется — ждёт следующего `recv`.
const CHUNK: usize = 64;

/// Что пошло не так при приёме или отправке кадра.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum CobsError<E> {
    /// Отказ потока.
    Io(E),
    /// Поток кончился: соединение закрыто. Потеря, а не отказ канала: над
    /// TCP-сервером, который принимает следующего клиента
    /// (`net::TcpServer`), следующий `recv` работает; поток, кончившийся
    /// насовсем, узел объявит отказавшим после серии потерь подряд.
    Eof,
    /// Кадр не разобрался как COBS.
    Corrupt,
    /// CRC кадра не сошлась.
    Crc,
    /// Кадр длиннее буфера: при приёме — буфера канала (ему нужно
    /// [`rx_len`]), при отправке — буфера кадра ([`tx_len`]).
    TooLong,
}

/// Сколько байт буфера отправки нужно на сообщение длиной `message`:
/// худший случай COBS, CRC и два разделителя.
#[must_use]
pub const fn tx_len(message: usize) -> usize {
    max_encoding_length(message + CRC_LEN) + 2
}

/// Сколько байт буфера приёма нужно на сообщение длиной `message`:
/// сообщение и CRC.
#[must_use]
pub const fn rx_len(message: usize) -> usize {
    message + CRC_LEN
}

/// Кадрированный канал поверх потока `io`.
///
/// Два буфера. `buf` — буфер канала (`FrameLink::buffer`): на сообщение
/// длиной `n` нужно [`rx_len`]`(n)` байт — CRC декодируется туда же, следом
/// за сообщением. `frame` — закодированный кадр отправки: на сообщение
/// длиной `n` нужно [`tx_len`]`(n)`.
///
/// Разорванное соединение (TCP) поток не восстанавливает: конец потока —
/// [`LinkError::Lost`] с [`CobsError::Eof`], а принять новое соединение — дело
/// потока под ним (`net::TcpServer` делает это на следующем чтении).
#[derive(Debug)]
pub struct Cobs<'b, T> {
    reader: Reader<T>,
    buf: &'b mut [u8],
    frame: &'b mut [u8],
    /// Кадр в потоке начат и не дочитан до `0x00`. Пока флаг стоит,
    /// очередной `recv` сначала отбрасывает хвост до `0x00`: если прошлый
    /// `recv` бросили на середине — даже на середине такого же пропуска, —
    /// хвост не примется за начало нового кадра. Снимается только
    /// прочитанным `0x00`.
    inside: bool,
}

/// Память канала COBS от приложения: буфер канала ([`rx_len`] от самого
/// длинного сообщения) и буфер кадра отправки ([`tx_len`] от буфера канала).
#[derive(Debug)]
pub struct CobsBuffers {
    /// Буфер канала (`FrameLink::buffer`).
    pub buf: &'static mut [u8],
    /// Закодированный кадр отправки.
    pub frame: &'static mut [u8],
}

/// Собирается над готовым потоком платы (UART, поток USB CDC).
impl<T> Assemble for Cobs<'static, T> {
    type Source = T;
    type Buffers = CobsBuffers;

    fn assemble(io: T, buffers: CobsBuffers) -> Self {
        Self::new(io, buffers.buf, buffers.frame)
    }
}
// {%- if has_tcp == "true" %}

/// COBS над TCP-сервером, который собирается над стеком платы.
///
/// Свой тип, а не ещё одна реализация [`Assemble`] у [`Cobs`]: у `Cobs`
/// источник — готовый поток, а `TcpServer` — тоже поток, и две реализации
/// пересеклись бы.
#[derive(Debug)]
pub struct TcpCobs(Cobs<'static, crate::net::TcpServer>);

/// Память [`TcpCobs`]: порт и буферы сокета, буферы канала COBS.
#[derive(Debug)]
pub struct TcpCobsBuffers {
    /// Сокет сервера.
    pub socket: crate::net::TcpServerBuffers,
    /// Канал COBS над ним.
    pub cobs: CobsBuffers,
}

impl Assemble for TcpCobs {
    type Source = embassy_net::Stack<'static>;
    type Buffers = TcpCobsBuffers;

    fn assemble(stack: embassy_net::Stack<'static>, buffers: TcpCobsBuffers) -> Self {
        let server = crate::net::TcpServer::assemble(stack, buffers.socket);
        Self(Cobs::assemble(server, buffers.cobs))
    }
}

impl FrameLink for TcpCobs {
    type Error = CobsError<crate::net::TcpError>;

    async fn recv(&mut self) -> Result<usize, LinkError<Self::Error>> {
        self.0.recv().await
    }

    fn buffer(&mut self) -> &mut [u8] {
        self.0.buffer()
    }

    async fn send(&mut self, len: usize) -> Result<(), LinkError<Self::Error>> {
        self.0.send(len).await
    }
}
// {%- endif %}

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
            buf,
            frame,
            inside: false,
        }
    }

    /// Поток обратно — например, чтобы закрыть сокет.
    pub fn into_inner(self) -> T {
        self.reader.io
    }
}

/// Отказ потока: разорванное соединение не вылечится следующим кадром, а
/// шум на UART (переполнение, чётность) — вылечится. `embassy-stm32` отдаёт
/// все ошибки UART как `ErrorKind::Other` — значит, «потерян кадр».
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
        // Хвост кадра, начатого до этого вызова, — не наш: до `0x00`.
        while self.inside {
            self.inside = self.reader.next_byte().await? != 0;
        }
        let mut decoder = CobsDecoder::new(&mut *self.buf);
        let len = loop {
            let byte = self.reader.next_byte().await?;
            self.inside = byte != 0;
            match decoder.feed(byte) {
                Ok(None) => {}
                Ok(Some(len)) => break len,
                Err(DecodeError::TargetBufTooSmall) => {
                    return Err(LinkError::Lost(CobsError::TooLong));
                }
                // `EmptyFrame` потоковый `feed` не возвращает (пустые кадры
                // между `0x00` он пропускает сам) — ветка для полноты.
                Err(DecodeError::InvalidFrame { .. } | DecodeError::EmptyFrame) => {
                    return Err(LinkError::Lost(CobsError::Corrupt));
                }
            }
        };
        let Some(message) = len.checked_sub(CRC_LEN) else {
            return Err(LinkError::Lost(CobsError::Corrupt));
        };
        let (body, crc) = self.buf[..len].split_at(message);
        if CRC.checksum(body).to_le_bytes() != *crc {
            return Err(LinkError::Lost(CobsError::Crc));
        }
        Ok(message)
    }

    fn buffer(&mut self) -> &mut [u8] {
        self.buf
    }

    async fn send(&mut self, len: usize) -> Result<(), LinkError<Self::Error>> {
        let message = &self.buf[..len];
        if tx_len(len) > self.frame.len() {
            return Err(LinkError::Lost(CobsError::TooLong));
        }
        let crc = CRC.checksum(message).to_le_bytes();
        self.frame[0] = 0;
        let mut encoder = CobsEncoder::new(&mut self.frame[1..]);
        // Размер проверен выше — места хватит на худший случай.
        encoder
            .push(message)
            .map_err(|_too_small| LinkError::Lost(CobsError::TooLong))?;
        encoder
            .push(&crc)
            .map_err(|_too_small| LinkError::Lost(CobsError::TooLong))?;
        let end = 1 + encoder.finalize();
        self.frame[end] = 0;
        let io = &mut self.reader.io;
        io.write_all(&self.frame[..=end]).await.map_err(io_error)?;
        io.flush().await.map_err(io_error)
    }
}

#[cfg(test)]
mod tests {
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
    use ports::{FrameLink, LinkError};

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

    /// Кадры сообщений так, как их отправляет `Cobs`.
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
    async fn frame_has_no_zero_inside_and_ends_with_zero() {
        let wire = frames(&[&[0, 1, 0, 2, 0]]).await;
        assert_eq!((wire.first(), wire.last()), (Some(&0), Some(&0)));
        assert!(!wire[1..wire.len() - 1].contains(&0), "кадр {wire:02X?}");
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

    #[tokio::test]
    async fn resyncs_after_garbage_and_corruption() {
        let good = frames(&[b"ok"]).await;
        let mut corrupted = good.clone();
        corrupted[1] ^= 0x40;
        let wire = [&[0x31, 0x32][..], &[0], &corrupted, &[0, 0], &good].concat();
        let (mut buf, mut frame) = ([0; 64], [0; 16]);
        let mut link = Cobs::new(Stream::new(wire, 64), &mut buf, &mut frame);
        let got = recv_all(&mut link).await;
        assert_eq!(got.last(), Some(&Ok(b"ok".to_vec())), "все {got:?}");
        assert!(
            got[..got.len() - 1]
                .iter()
                .all(|r| matches!(r, Err(LinkError::Lost(_)))),
            "до хорошего кадра — только потери: {got:?}"
        );
    }

    #[tokio::test]
    async fn too_long_frame_is_lost_and_the_next_is_received() {
        let wire = frames(&[&[7; 100], b"next"]).await;
        let (mut buf, mut frame) = ([0; 64], [0; 16]);
        let mut link = Cobs::new(Stream::new(wire, 64), &mut buf, &mut frame);
        let got = recv_all(&mut link).await;
        let want: Vec<Received> = vec![
            Err(LinkError::Lost(CobsError::TooLong)),
            Ok(b"next".to_vec()),
        ];
        assert_eq!(got, want);
    }

    #[tokio::test]
    async fn dropped_recv_does_not_glue_the_tail_to_the_next_frame() {
        let first = frames(&[b"interrupted frame"]).await;
        let wire = [first.clone(), frames(&[b"whole"]).await].concat();
        let mut stream = Stream::new(wire, 64);
        stream.block_at = Some(first.len() / 2);
        let (mut buf, mut frame) = ([0; 64], [0; 16]);
        let mut link = Cobs::new(stream, &mut buf, &mut frame);
        {
            let mut recv = pin!(link.recv());
            let mut cx = Context::from_waker(Waker::noop());
            assert!(recv.as_mut().poll(&mut cx).is_pending());
        }
        link.reader.io.block_at = None;
        let len = link.recv().await.unwrap();
        assert_eq!(&link.buffer()[..len], b"whole");
    }

    /// Второй брошенный `recv` — посреди пропуска хвоста первого — не
    /// снимает пропуск: остаток хвоста всё равно не примется за кадр.
    #[tokio::test]
    async fn recv_dropped_while_skipping_keeps_skipping() {
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
        assert_eq!(&link.buffer()[..len], b"whole");
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

    proptest::proptest! {
        /// Любые сообщения, любое дробление потока — на выходе те же сообщения.
        #[test]
        fn roundtrip(
            messages in proptest::collection::vec(
                proptest::collection::vec(proptest::num::u8::ANY, 0..60),
                1..6,
            ),
            max in 1_usize..80,
        ) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let got = rt.block_on(async {
                let refs: Vec<&[u8]> = messages.iter().map(Vec::as_slice).collect();
                let wire = frames(&refs).await;
                let (mut buf, mut frame) = ([0; 64], [0; 16]);
                let mut link = Cobs::new(Stream::new(wire, max), &mut buf, &mut frame);
                recv_all(&mut link).await
            });
            let want: Vec<Received> = messages.into_iter().map(Ok).collect();
            proptest::prop_assert_eq!(got, want);
        }

        /// Мусор на линии — только потери кадров, без паники.
        #[test]
        fn garbage_never_panics(wire in proptest::collection::vec(proptest::num::u8::ANY, 0..500)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let (mut buf, mut frame) = ([0; 64], [0; 16]);
                let mut link = Cobs::new(Stream::new(wire, 7), &mut buf, &mut frame);
                recv_all(&mut link).await
            });
        }
    }
}
