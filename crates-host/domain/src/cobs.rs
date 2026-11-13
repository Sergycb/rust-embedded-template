//! Sans-IO кодек кадрирования потока: COBS и CRC-16/IBM-SDLC (X.25, FCS у
//! HDLC и PPP), младшим байтом первым.
//!
//! Кадр на проводе: `0x00`, COBS от `[сообщение][CRC]`, `0x00`.
//!
//! * **COBS** переписывает данные так, что в них нет `0x00`, поэтому `0x00`
//!   однозначно отмечает границу кадра. Приёмник, потерявший синхронизацию
//!   (помеха, включился посреди передачи, слишком длинный кадр), ждёт
//!   следующий `0x00` — и с него снова читает кадры. Накладные расходы — байт
//!   на 254.
//! * **`0x00` и в начале кадра**: если отправку бросили на середине (таймаут
//!   у вызывающего), обрывок без конца не склеится со следующим кадром — его
//!   отрежет ведущий `0x00` следующего. Пустые кадры между `0x00` приёмник
//!   пропускает.
//! * **CRC** отсекает искажённый и обрезанный кадр: без неё protobuf разобрал
//!   бы его молча, с неверными полями.
//!
//! Здесь — только логика кадра, без I/O: [`Codec`] держит состояние приёма,
//! вызывающий гоняет байты потока через [`Codec::feed`] сам, поэтому брошенный
//! посреди кадра приём ничего не теряет (буфер у вызывающего, а не в future).
//! Транспорт поверх `embedded_io_async::{Read, Write}` — в
//! `adapters_logic::cobs` (`ports::FrameLink`). Хост собирает кадр так же:
//! `b"\0" + cobs_encode(message + crc_le) + b"\0"`.

use ::cobs::{CobsEncoder, decode_in_place, max_encoding_length};
use crc::{CRC_16_IBM_SDLC, Crc, NoTable};

/// CRC кадра. Без таблицы: 512 байт флеша не стоят выигрыша в скорости на
/// кадрах в сотни байт.
const CRC: Crc<u16, NoTable> = Crc::<u16, NoTable>::new(&CRC_16_IBM_SDLC);

/// Длина CRC в конце кадра.
pub const CRC_LEN: usize = 2;

/// Потеря кадра: декодирование или CRC. Без отказов транспорта — их добавляет
/// драйвер (`adapters_logic::cobs::CobsError`).
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    /// Кадр не разобрался как COBS.
    Corrupt,
    /// CRC кадра не сошлась.
    Crc,
    /// Кадр длиннее буфера: при приёме — буфера канала (ему нужно
    /// [`rx_len`]), при отправке — буфера кадра ([`tx_len`]).
    TooLong,
}

/// Состояние приёма кадров: сколько байт начатого (ещё закодированного) кадра
/// уже в буфере и отбрасывается ли остаток потерянного кадра до `0x00`.
///
/// Буфера не держит: `feed` пишет в буфер вызывающего, и состояние между
/// вызовами живёт здесь, а не в future приёма — брошенный посреди кадра `recv`
/// продолжится с того же байта следующим.
#[derive(Debug, Default)]
pub struct Codec {
    /// Сколько байт начатого (ещё закодированного) кадра уже в буфере.
    pos: usize,
    /// Остаток потерянного кадра отбрасывается до `0x00`: иначе он был бы
    /// принят за начало нового.
    skipping: bool,
}

impl Codec {
    /// Чистый кодек: ни начатого кадра, ни потерянного хвоста.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pos: 0,
            skipping: false,
        }
    }

    /// В начатом кадре есть принятые байты.
    #[must_use]
    pub const fn is_started(&self) -> bool {
        self.pos > 0
    }

    /// Отбрасывается ли сейчас остаток потерянного кадра.
    #[must_use]
    pub const fn is_skipping(&self) -> bool {
        self.skipping
    }

    /// Начатый кадр потерян: копить — с начала; остаток кадра в потоке, если
    /// он там есть, отбрасывается до `0x00`.
    pub fn drop_frame(&mut self, rest_in_stream: bool) {
        self.skipping = rest_in_stream;
        self.pos = 0;
    }

    /// Байт потока — в начатый кадр `buf`; на `0x00` кадр декодируется на
    /// месте. `None` — байт съеден, кадр не готов; `Some(Ok(len))` — кадр
    /// собран, `len` — длина с CRC (сообщение — после [`decode`]);
    /// `Some(Err(_))` — кадр потерян, приём продолжается с следующего.
    pub fn feed(&mut self, byte: u8, buf: &mut [u8]) -> Option<Result<usize, Error>> {
        if self.skipping {
            self.skipping = byte != 0;
            return None;
        }
        if byte != 0 {
            let Some(slot) = buf.get_mut(self.pos) else {
                // Байт данных — не `0x00`: кадр в потоке продолжается.
                self.drop_frame(true);
                return Some(Err(Error::TooLong));
            };
            *slot = byte;
            self.pos += 1;
            return None;
        }
        // Пустой кадр между двумя `0x00` — ведущий разделитель, а не кадр.
        let encoded = core::mem::take(&mut self.pos);
        (encoded > 0).then(|| {
            decode_in_place(&mut buf[..encoded])
                .map_err(|_corrupt| Error::Corrupt)
        })
    }
}

/// Принятый кадр (в `buf`, длина `len` с CRC) — сообщение: CRC сверяется и
/// отрезается. `Ok` — длина сообщения, данные на месте в начале `buf`.
///
/// # Errors
///
/// [`Error::Corrupt`] — кадр короче CRC; [`Error::Crc`] — CRC не сошлась.
pub fn decode(buf: &[u8], len: usize) -> Result<usize, Error> {
    let Some(message) = len.checked_sub(CRC_LEN) else {
        return Err(Error::Corrupt);
    };
    let (body, crc) = buf[..len].split_at(message);
    if CRC.checksum(body).to_le_bytes() != *crc {
        return Err(Error::Crc);
    }
    Ok(message)
}

/// Кодирует `message` с CRC в кадр в `frame` и возвращает длину кадра:
/// `0x00`, COBS, `0x00`. Размеры буферов — [`tx_len`] на кадр отправки.
///
/// # Errors
///
/// [`Error::TooLong`] — кадр не влезает в `frame`: ничего не записано.
pub fn encode(frame: &mut [u8], message: &[u8]) -> Result<usize, Error> {
    if tx_len(message.len()) > frame.len() {
        return Err(Error::TooLong);
    }
    let crc = CRC.checksum(message).to_le_bytes();
    frame[0] = 0;
    let mut encoder = CobsEncoder::new(&mut frame[1..]);
    // Размер проверен выше — места хватит на худший случай.
    encoder
        .push(message)
        .map_err(|_too_small| Error::TooLong)?;
    encoder
        .push(&crc)
        .map_err(|_too_small| Error::TooLong)?;
    let end = 1 + encoder.finalize();
    frame[end] = 0;
    Ok(end + 1)
}

/// Сколько байт буфера отправки нужно на сообщение длиной `message`:
/// худший случай COBS, CRC и два разделителя.
#[must_use]
pub const fn tx_len(message: usize) -> usize {
    max_encoding_length(message + CRC_LEN) + 2
}

/// Сколько байт буфера приёма нужно на сообщение длиной `message`:
/// закодированные сообщение и CRC — кадр копится в буфере до декодирования.
#[must_use]
pub const fn rx_len(message: usize) -> usize {
    max_encoding_length(message + CRC_LEN)
}

#[cfg(test)]
mod tests {
    use super::{Codec, decode, encode};

    /// Кадр сообщений так, как его отправляет [`encode`].
    fn wire(messages: &[&[u8]]) -> Vec<u8> {
        let mut wire = Vec::new();
        let mut frame = [0; 256];
        for message in messages {
            let len = encode(&mut frame, message).unwrap();
            wire.extend_from_slice(&frame[..len]);
        }
        wire
    }

    /// Всё, что [`Codec::feed`] отдаёт по байтам `input`: кадры (длина с CRC)
    /// и потери.
    fn receive(input: &[u8]) -> Vec<Result<usize, super::Error>> {
        let mut buf = [0; 64];
        let mut codec = Codec::new();
        let mut out = Vec::new();
        for &byte in input {
            if let Some(outcome) = codec.feed(byte, &mut buf) {
                out.push(outcome);
            }
        }
        out
    }

    #[test]
    fn frame_has_no_zero_inside_and_ends_with_zero() {
        let mut frame = [0; 64];
        let len = encode(&mut frame, &[0, 1, 0, 2, 0]).unwrap();
        assert_eq!((frame.first(), frame.get(len - 1)), (Some(&0), Some(&0)));
        assert!(!frame[1..len - 1].contains(&0), "кадр {frame:02X?}");
    }

    #[test]
    fn roundtrip_recovers_messages() {
        let wire = wire(&[b"first", b"", b"third message"]);
        let mut buf = [0; 64];
        let mut codec = Codec::new();
        let mut got = Vec::new();
        for &byte in &wire {
            if let Some(Ok(len)) = codec.feed(byte, &mut buf) {
                let message = decode(&buf, len).unwrap();
                got.push(buf[..message].to_vec());
            }
        }
        assert_eq!(got, [b"first".to_vec(), vec![], b"third message".to_vec()]);
    }

    /// Мусор и искажённый кадр — только потери, дальше хороший кадр
    /// принимается: синхронизация возвращается на `0x00`.
    #[test]
    fn resyncs_after_garbage_and_corruption() {
        let good = wire(&[b"ok"]);
        let mut corrupted = good.clone();
        corrupted[1] ^= 0x40;
        let input = [&[0x31, 0x32][..], &[0], &corrupted, &[0, 0], &good].concat();
        let mut buf = [0; 64];
        let mut codec = Codec::new();
        let mut got: Vec<Result<Vec<u8>, super::Error>> = Vec::new();
        for &byte in &input {
            if let Some(outcome) = codec.feed(byte, &mut buf) {
                got.push(
                    outcome.and_then(|len| decode(&buf, len).map(|m| buf[..m].to_vec())),
                );
            }
        }
        assert_eq!(got.last(), Some(&Ok(b"ok".to_vec())), "все {got:?}");
        assert!(
            got[..got.len() - 1].iter().all(|r| r.is_err()),
            "до хорошего кадра — только потери: {got:?}"
        );
    }

    /// Кадр длиннее буфера — потеря, и приём следующего кадра не сломан.
    #[test]
    fn too_long_frame_is_lost_and_the_next_is_received() {
        let input = wire(&[&[7; 100], b"next"]);
        let got = receive(&input);
        let want = vec![Err(super::Error::TooLong), Ok(6)];
        assert_eq!(got, want);
    }

    proptest::proptest! {
        /// Любые сообщения — круг восстанавливает их.
        #[test]
        fn roundtrip(
            messages in proptest::collection::vec(
                proptest::collection::vec(proptest::num::u8::ANY, 0..60),
                1..6,
            ),
        ) {
            let refs: Vec<&[u8]> = messages.iter().map(Vec::as_slice).collect();
            let input = wire(&refs);
            let mut buf = [0; 64];
            let mut codec = Codec::new();
            let mut got = Vec::new();
            for &byte in &input {
                if let Some(Ok(len)) = codec.feed(byte, &mut buf) {
                    let message = decode(&buf, len).unwrap();
                    got.push(buf[..message].to_vec());
                }
            }
            proptest::prop_assert_eq!(got, messages);
        }

        /// Мусор на линии — только потери кадров, без паники.
        #[test]
        fn garbage_never_panics(input in proptest::collection::vec(proptest::num::u8::ANY, 0..500)) {
            let _ = receive(&input);
        }
    }
}
