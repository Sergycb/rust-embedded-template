//! Кадр Modbus RTU — адрес, PDU, CRC16 — поверх потока байтов.

use crc::{CRC_16_MODBUS, Crc, NoTable};
use embedded_io_async::{Read, ReadExactError, Write};
use ports::{Assemble, FrameLink, LinkError, MAX_PDU};

// {%- if modbus_master == "true" %}
use ports::{MasterError, ModbusClient};

use super::master_error;
// {%- endif %}
use super::{FrameError, io_error, read_error};

/// Наибольший кадр RTU: адрес, PDU до 253 байт, CRC.
const MAX_ADU: usize = MAX_PDU + 3;

// {%- if modbus_master == "true" %}
/// Сколько байт слива хватит, чтобы убрать опоздавший ответ и хвост битого
/// кадра, — не больше двух кадров. Предел нужен, чтобы линия, на которой
/// приём не кончается (шум, второй master), не держала запрос вечно.
const MAX_DRAIN: usize = 2 * MAX_ADU;
// {%- endif %}

/// Modbus RTU поверх потока байтов `io` (`BufferedUart` с DE для RS-485).
///
/// Slave ([`FrameLink`], кадр «адрес и PDU») не принимает кадр с неверной CRC и кадр с
/// функцией, кроме FC01–06, 15, 16: длину её кадра по коду функции не
/// вывести, а отвечать, не проверив CRC, нельзя — master получит таймаут, а
/// не Illegal Function. Кадры на зарезервированные адреса 248–255
/// пропускаются.
///
// {%- if modbus_master == "true" %}
/// Master ([`ModbusClient`]) перед каждым запросом сливает всё, что уже
/// лежит во входном буфере (`ReadReady` — его реализуют `BufferedUart` и
/// `RingBufferedUartRx` из `embassy-stm32`): брошенный на таймауте запрос
/// оставляет в потоке опоздавший ответ, неразобранный кадр — свой хвост.
/// Не закрыт один случай: ответ, который ещё в пути, когда уходит следующий
/// запрос, — тот будет прочитан как ответ на новый. Таймаут поэтому стоит
/// брать с запасом на время ответа самого медленного slave'а.
///
// {%- endif %}
/// На общей шине RS-485 slave видит и ответы других slave'ов, и чужой ответ
/// может на время сбить его с границы кадра, пока CRC не выровняет поток.
#[derive(Debug)]
pub struct Rtu<T> {
    io: T,
    /// Принятые, но ещё не разобранные байты — `rx[..have]`.
    rx: [u8; MAX_ADU],
    have: usize,
    /// Буфер канала: принятый кадр «адрес и PDU», затем ответ — и место под
    /// CRC за ним. Отдельно от `rx`: там за кадром могут лежать уже принятые
    /// байты следующего, и ответ поверх них их бы затёр.
    frame: [u8; MAX_ADU],
}

impl<T> Rtu<T> {
    /// Транспорт поверх потока `io`.
    pub const fn new(io: T) -> Self {
        Self {
            io,
            rx: [0; MAX_ADU],
            have: 0,
            frame: [0; MAX_ADU],
        }
    }
}

impl<T> Assemble for Rtu<T> {
    type Source = T;
    type Buffers = ();

    fn assemble(io: T, (): ()) -> Self {
        Self::new(io)
    }
}

impl<T: Read + Write> FrameLink for Rtu<T> {
    type Error = FrameError<T::Error>;

    async fn recv(&mut self) -> Result<usize, LinkError<Self::Error>> {
        loop {
            let len = self.next_request().await.map_err(read_error)?;
            let body = len - 2;
            let reserved = self.rx[0] > 247;
            if !reserved {
                self.frame[..body].copy_from_slice(&self.rx[..body]);
            }
            self.consume(len);
            if !reserved {
                return Ok(body);
            }
        }
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.frame[..MAX_ADU - 2]
    }

    /// Ответ — с адресом, который обработчик оставил в начале кадра.
    async fn send(&mut self, len: usize) -> Result<(), LinkError<Self::Error>> {
        let total = seal(&mut self.frame, len);
        self.io
            .write_all(&self.frame[..total])
            .await
            .map_err(io_error)?;
        self.io.flush().await.map_err(io_error)
    }
}

// {%- if modbus_master == "true" %}
impl<T: Read + embedded_io_async::ReadReady + Write> ModbusClient for Rtu<T> {
    type Error = T::Error;

    async fn exchange(
        &mut self,
        unit: u8,
        request: &[u8],
        response: &mut [u8],
    ) -> Result<usize, MasterError<T::Error>> {
        assert!(
            (1..=247).contains(&unit),
            "запрос на адрес {unit}: у slave'а RTU адрес 1..=247"
        );
        assert!(
            response.len() >= MAX_PDU,
            "буфер PDU {} < {MAX_PDU}",
            response.len()
        );
        self.send_request(unit, request).await?;
        let len = self.receive().await?;
        if self.rx[0] != unit {
            return Err(MasterError::Unexpected);
        }
        let pdu = &self.rx[1..len - 2];
        response[..pdu.len()].copy_from_slice(pdu);
        Ok(pdu.len())
    }

    async fn broadcast(&mut self, request: &[u8]) -> Result<(), MasterError<T::Error>> {
        self.send_request(0, request).await
    }
}
// {%- endif %}

impl<T: Read> Rtu<T> {
    /// Ждёт кадр запроса с верной CRC в начале `rx`; возвращает его длину.
    ///
    /// Читает ровно столько, сколько не хватает до кадра, — ни байтом
    /// больше, иначе начало следующего кадра пришлось бы хранить где-то ещё.
    async fn next_request(&mut self) -> Result<usize, ReadExactError<T::Error>> {
        loop {
            self.fill(2).await?;
            let Some(header) = request_header(self.rx[1]) else {
                self.consume(1);
                continue;
            };
            self.fill(header).await?;
            let len = request_len(&self.rx[..header]);
            if len <= MAX_ADU {
                self.fill(len).await?;
                if crc_ok(&self.rx[..len]) {
                    return Ok(len);
                }
            }
            self.consume(1);
        }
    }

    async fn fill(&mut self, want: usize) -> Result<(), ReadExactError<T::Error>> {
        super::fill(&mut self.io, &mut self.rx, &mut self.have, want).await
    }

    /// Убирает `n` байт из начала принятого.
    fn consume(&mut self, n: usize) {
        self.rx.copy_within(n..self.have, 0);
        self.have -= n;
    }

    // {%- if modbus_master == "true" %}
    /// Принимает один кадр ответа в начало `rx`; возвращает его длину.
    async fn receive(&mut self) -> Result<usize, MasterError<T::Error>> {
        let rx = &mut self.rx;
        self.io
            .read_exact(&mut rx[..2])
            .await
            .map_err(master_error)?;
        let header = response_header(rx[1]).ok_or(MasterError::Unexpected)?;
        self.io
            .read_exact(&mut rx[2..header])
            .await
            .map_err(master_error)?;
        let len = response_len(&rx[..header]);
        if len > MAX_ADU {
            return Err(MasterError::Unexpected);
        }
        self.io
            .read_exact(&mut rx[header..len])
            .await
            .map_err(master_error)?;
        if crc_ok(&rx[..len]) {
            Ok(len)
        } else {
            Err(MasterError::Crc)
        }
    }
    // {%- endif %}
}

// {%- if modbus_master == "true" %}
impl<T: Read + embedded_io_async::ReadReady + Write> Rtu<T> {
    /// Сливает входной буфер и отправляет кадр запроса.
    async fn send_request(
        &mut self,
        unit: u8,
        request: &[u8],
    ) -> Result<(), MasterError<T::Error>> {
        self.drain().await?;
        let mut tx = [0; MAX_ADU];
        let total = frame(&mut tx, unit, request);
        self.io
            .write_all(&tx[..total])
            .await
            .map_err(MasterError::Io)?;
        self.io.flush().await.map_err(MasterError::Io)
    }

    /// Выбрасывает то, что уже принято: опоздавший ответ на брошенный запрос
    /// или хвост неразобранного кадра. Иначе ответ на этот запрос читался бы
    /// со сдвигом, а опоздавший ответ на чтение был бы принят за свой — с
    /// верной CRC и чужими данными.
    async fn drain(&mut self) -> Result<(), MasterError<T::Error>> {
        let mut scratch = [0_u8; 32];
        let mut drained = 0;
        while drained < MAX_DRAIN && self.io.read_ready().map_err(MasterError::Io)? {
            let n = self.io.read(&mut scratch).await.map_err(MasterError::Io)?;
            if n == 0 {
                // Конец потока: дальше сливать нечего, а заметит его приём ответа.
                break;
            }
            drained += n;
        }
        Ok(())
    }
}
// {%- endif %}

// {%- if modbus_master == "true" %}
/// Собирает в `tx` кадр «`unit`, `pdu`, CRC»; возвращает его длину.
fn frame(tx: &mut [u8; MAX_ADU], unit: u8, pdu: &[u8]) -> usize {
    assert!(pdu.len() <= MAX_PDU, "PDU {} > {MAX_PDU} байт", pdu.len());
    tx[0] = unit;
    tx[1..=pdu.len()].copy_from_slice(pdu);
    seal(tx, 1 + pdu.len())
}
// {%- endif %}

/// Сколько первых байт запроса нужно, чтобы узнать длину кадра; `None` —
/// функция, длину кадра которой по одному коду функции не вывести.
fn request_header(fc: u8) -> Option<usize> {
    match fc {
        0x01..=0x06 => Some(2),
        // адрес, функция, начало, количество, счётчик байт
        0x0F | 0x10 => Some(7),
        _ => None,
    }
}

/// Длина кадра запроса по его заголовку из [`request_header`] байт:
/// FC01–06 — фиксированная, FC15/16 — по счётчику байт. Может превышать
/// [`MAX_ADU`] — такой кадр не настоящий.
fn request_len(header: &[u8]) -> usize {
    match header[1] {
        0x0F | 0x10 => 9 + usize::from(header[6]),
        _ => 8,
    }
}

// {%- if modbus_master == "true" %}
/// Сколько первых байт ответа нужно, чтобы узнать длину кадра; `None` —
/// функция, на которую master не отправляет запросов.
fn response_header(fc: u8) -> Option<usize> {
    match fc {
        _ if fc & 0x80 != 0 => Some(2),
        // адрес, функция, счётчик байт
        0x01..=0x04 => Some(3),
        0x05 | 0x06 | 0x0F | 0x10 => Some(2),
        _ => None,
    }
}

/// Длина кадра ответа по его заголовку из [`response_header`] байт:
/// исключение — 5, чтения — по счётчику байт, записи — эхо в 8. Может
/// превышать [`MAX_ADU`] — такой кадр не настоящий.
fn response_len(header: &[u8]) -> usize {
    match header[1] {
        fc if fc & 0x80 != 0 => 5,
        0x01..=0x04 => 5 + usize::from(header[2]),
        _ => 8,
    }
}
// {%- endif %}

/// CRC-16/MODBUS: полином 0xA001 (отражённый 0x8005), начальное значение
/// 0xFFFF. Без таблицы: 512 байт флеша не стоят выигрыша на кадрах до 256
/// байт.
const CRC: Crc<u16, NoTable> = Crc::<u16, NoTable>::new(&CRC_16_MODBUS);

/// Сошлась ли CRC в последних двух байтах кадра (младший байт первым).
fn crc_ok(frame: &[u8]) -> bool {
    match frame.len().checked_sub(2) {
        Some(body) if body > 0 => {
            CRC.checksum(&frame[..body]).to_le_bytes() == [frame[body], frame[body + 1]]
        }
        _ => false,
    }
}

/// Приписывает CRC к первым `len` байтам `buf`; возвращает длину кадра.
pub(super) fn seal(buf: &mut [u8], len: usize) -> usize {
    let crc = CRC.checksum(&buf[..len]).to_le_bytes();
    buf[len..len + 2].copy_from_slice(&crc);
    len + 2
}

#[cfg(test)]
mod tests {
    // {%- if modbus_master == "true" %}
    use core::convert::Infallible;

    // {%- endif %}
    use super::*;
    use crate::modbus::fakes::{Duplex, rtu_frame as frame};

    /// Кадры с CRC, посчитанной не нами: документация `async-modbus` и
    /// примеры спецификации.
    #[test]
    fn crc_matches_known_frames() {
        for frame in [
            &[0x01, 0x06, 0x10, 0xBC, 0x30, 0x39, 0x98, 0xFC][..],
            &[0x01, 0x03, 0x10, 0x01, 0x03, 0xE8, 0x10, 0x74],
            &[0x01, 0x06, 0x00, 0x04, 0x00, 0x02, 0x49, 0xCA],
            &[0x01, 0x03, 0x00, 0x6B, 0x00, 0x03, 0x74, 0x17],
        ] {
            assert!(crc_ok(frame), "CRC не сошлась на {frame:02X?}");
        }
    }

    #[test]
    fn seal_then_crc_ok() {
        let mut buf = [0x11, 0x03, 0x00, 0x6B, 0x00, 0x03, 0, 0];
        assert_eq!(seal(&mut buf, 6), 8);
        assert!(crc_ok(&buf));
        buf[3] ^= 1;
        assert!(!crc_ok(&buf));
    }

    #[test]
    fn crc_ok_rejects_too_short() {
        assert!(!crc_ok(&[]));
        assert!(!crc_ok(&[0xFF, 0xFF]));
    }

    #[test]
    fn request_lengths() {
        assert_eq!(request_header(0x03), Some(2));
        assert_eq!(request_len(&[1, 0x03]), 8);
        assert_eq!(request_header(0x10), Some(7));
        assert_eq!(request_len(&[1, 0x10, 0, 0, 0, 2, 4]), 13);
        assert_eq!(request_header(0x08), None);
    }

    // {%- if modbus_master == "true" %}
    #[test]
    fn response_lengths() {
        assert_eq!(response_header(0x83), Some(2));
        assert_eq!(response_len(&[1, 0x83]), 5);
        assert_eq!(response_header(0x03), Some(3));
        assert_eq!(response_len(&[1, 0x03, 6]), 11);
        assert_eq!(response_len(&[1, 0x10]), 8);
        assert_eq!(response_header(0x2B), None);
    }
    // {%- endif %}

    /// Все запросы из `input` до конца потока: адрес и PDU.
    async fn requests(input: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut rtu = Rtu::new(Duplex::new(input));
        let mut got = Vec::new();
        loop {
            match rtu.recv().await {
                Ok(len) => {
                    let frame = &rtu.buffer()[..len];
                    got.push((frame[0], frame[1..].to_vec()));
                }
                Err(e) => {
                    assert_eq!(e, LinkError::Lost(FrameError::Eof));
                    return got;
                }
            }
        }
    }

    #[tokio::test]
    async fn server_receives_and_replies() {
        let mut rtu = Rtu::new(Duplex::new(&frame(7, &[0x03, 0, 1, 0, 1])));
        let len = rtu.recv().await.unwrap();
        assert_eq!(&rtu.buffer()[..len], &[7, 0x03, 0, 1, 0, 1]);
        rtu.buffer()[1..5].copy_from_slice(&[0x03, 2, 0, 5]);
        rtu.send(5).await.unwrap();
        assert_eq!(rtu.io.tx, frame(7, &[0x03, 2, 0, 5]));
    }

    #[tokio::test]
    async fn server_resyncs_after_garbage_and_bad_crc() {
        let good = frame(1, &[0x03, 0, 0, 0, 1]);
        let mut broken = good.clone();
        broken[3] ^= 0xFF;
        let input = [&[0x55, 0x01, 0xAA][..], &broken, &good].concat();
        assert_eq!(requests(&input).await, [(1, vec![0x03, 0, 0, 0, 1])]);
    }

    #[tokio::test]
    async fn server_passes_any_unit_but_reserved() {
        let input = [
            frame(2, &[0x06, 0, 0, 0, 7]),
            frame(0, &[0x06, 0, 1, 0, 9]),
            frame(0xF8, &[0x06, 0, 2, 0, 1]),
        ]
        .concat();
        let units: Vec<_> = requests(&input).await.into_iter().map(|r| r.0).collect();
        assert_eq!(units, [2, 0]);
    }

    // {%- if modbus_master == "true" %}
    /// Обмен по RTU с `reply` в ответ; возвращает исход и отправленное.
    async fn exchange(
        unit: u8,
        request: &[u8],
        io: Duplex,
    ) -> (Result<Vec<u8>, MasterError<Infallible>>, Vec<u8>) {
        let mut rtu = Rtu::new(io);
        let mut out = [0; MAX_PDU];
        let r = rtu.exchange(unit, request, &mut out).await;
        (r.map(|len| out[..len].to_vec()), rtu.io.tx)
    }

    #[tokio::test]
    async fn client_request_bytes_are_spec_example() {
        // Пример из спецификации: прочитать 3 регистра с 0x006B у slave'а 17.
        let (_, tx) = exchange(0x11, &[0x03, 0x00, 0x6B, 0x00, 0x03], Duplex::replying(&[])).await;
        assert_eq!(tx, [0x11, 0x03, 0x00, 0x6B, 0x00, 0x03, 0x76, 0x87]);
    }

    #[tokio::test]
    async fn client_stale_reply_is_drained_not_taken() {
        // Опоздавший ответ на брошенный запрос уже лежит в буфере; свой
        // приходит после отправки — его master и должен вернуть.
        let stale = frame(1, &[0x03, 2, 0, 1]);
        let fresh = frame(1, &[0x03, 2, 0, 2]);
        let io = Duplex::replying(&fresh).with_stale(&stale);
        let (got, _) = exchange(1, &[0x03, 0, 100, 0, 1], io).await;
        assert_eq!(got, Ok(vec![0x03, 2, 0, 2]));
    }

    #[tokio::test]
    async fn client_rejects_broken_replies() {
        let mut bad_crc = frame(1, &[0x03, 2, 0, 7]);
        bad_crc[3] ^= 1;
        let cases = [
            (bad_crc.clone(), MasterError::Crc),
            (bad_crc[..4].to_vec(), MasterError::Eof),
            // ответ другого slave'а
            (frame(2, &[0x03, 2, 0, 7]), MasterError::Unexpected),
            // функция, длину ответа которой не вывести
            (frame(1, &[0x2B, 0x0E]), MasterError::Unexpected),
        ];
        for (reply, error) in cases {
            let (got, _) = exchange(1, &[0x03, 0, 0, 0, 1], Duplex::replying(&reply)).await;
            assert_eq!(got, Err(error), "ответ {reply:02X?}");
        }
    }

    #[tokio::test]
    async fn broadcast_does_not_wait() {
        let mut rtu = Rtu::new(Duplex::replying(&[]));
        assert_eq!(rtu.broadcast(&[0x06, 0, 1, 0, 2]).await, Ok(()));
        assert_eq!(rtu.io.tx, frame(0, &[0x06, 0, 1, 0, 2]));
    }
    // {%- endif %}

    proptest::proptest! {
        /// Что бы ни пришло по линии — slave не паникует и кончается только
        /// концом потока.
        #[test]
        fn server_never_panics(input in proptest::collection::vec(proptest::num::u8::ANY, 0..600)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(requests(&input));
        }

        // {%- if modbus_master == "true" %}
        /// Какой бы ответ ни пришёл — master не паникует.
        #[test]
        fn client_never_panics(reply in proptest::collection::vec(proptest::num::u8::ANY, 0..300)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let _ignored = rt.block_on(exchange(1, &[0x03, 0, 0, 0, 4], Duplex::replying(&reply)));
        }
        // {%- endif %}
    }
}
