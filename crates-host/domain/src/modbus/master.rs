//! Modbus RTU master поверх потока байтов: по функции на запрос.
//!
//! Каждая функция отправляет запрос и читает ровно один кадр ответа — ни
//! байтом больше. Ответ из-за линии — граница доверия: он сверяется с
//! запросом (адрес, функция, количество, счётчик байт, эхо записи), и всё,
//! что не сошлось, — [`MasterError::Unexpected`](ports::MasterError::Unexpected),
//! а не паника и не мусор в `out`.
//!
//! Таймаута здесь нет: ждать ответ сколько угодно — дело вызывающего
//! (`embassy_time::with_timeout`). Брошенный на таймауте запрос оставляет
//! в потоке опоздавший ответ, а неразобранный кадр — свой хвост; поэтому
//! перед каждым запросом master сливает всё, что уже лежит во входном
//! буфере (`ReadReady` — его реализуют `BufferedUart` и
//! `RingBufferedUartRx` из `embassy-stm32`). Не закрыт один случай: ответ,
//! который ещё в пути, когда уходит следующий запрос, — тот будет прочитан
//! как ответ на новый. Таймаут поэтому стоит брать с запасом на время
//! ответа самого медленного slave'а.
//!
//! После широковещательной записи (`unit` 0) ответа нет, и функция
//! возвращается сразу; выдержать паузу, пока slave'ы её выполняют
//! (turnaround delay спецификации, обычно 100–200 мс), — тоже дело
//! вызывающего.

use embedded_io_async::{Read, ReadExactError, ReadReady, Write};
use ports::{Bits, Exception, MasterError, Words};
use rusty_modbus_codec::{ResponsePdu, decode_response};
use rusty_modbus_types::{
    MAX_READ_COILS, MAX_READ_DISCRETE_INPUTS, MAX_READ_REGISTERS, MAX_WRITE_COILS,
    MAX_WRITE_REGISTERS,
};

use super::rtu::{self, MAX_ADU};

/// Кадр запроса фиксированной длины (FC01–06): адрес, функция, два
/// 16-битных поля, CRC.
const FIXED_REQUEST: usize = 8;

/// Сколько байт слива хватит, чтобы убрать опоздавший ответ и хвост битого
/// кадра, — не больше двух кадров. Предел нужен, чтобы линия, на которой
/// приём не кончается (шум, второй master), не держала запрос вечно.
const MAX_DRAIN: usize = 2 * MAX_ADU;

/// Читает coils `start..start + out.len()` slave'а `unit` в `out` (FC01).
///
/// # Errors
///
/// Отказ транспорта, исключение slave'а, ответ не на этот запрос.
///
/// # Panics
///
/// Если `unit` не 1..=247 или `out.len()` не 1..=2000.
pub async fn read_coils<T: Read + ReadReady + Write>(
    io: &mut T,
    unit: u8,
    start: u16,
    out: &mut [bool],
) -> Result<(), MasterError<T::Error>> {
    read_bits(io, unit, 0x01, start, out, MAX_READ_COILS).await
}

/// Читает discrete inputs `start..start + out.len()` slave'а `unit` в `out`
/// (FC02).
///
/// # Errors
///
/// Отказ транспорта, исключение slave'а, ответ не на этот запрос.
///
/// # Panics
///
/// Если `unit` не 1..=247 или `out.len()` не 1..=2000.
pub async fn read_discrete_inputs<T: Read + ReadReady + Write>(
    io: &mut T,
    unit: u8,
    start: u16,
    out: &mut [bool],
) -> Result<(), MasterError<T::Error>> {
    read_bits(io, unit, 0x02, start, out, MAX_READ_DISCRETE_INPUTS).await
}

/// Читает holding registers `start..start + out.len()` slave'а `unit` в
/// `out` (FC03).
///
/// # Errors
///
/// Отказ транспорта, исключение slave'а, ответ не на этот запрос.
///
/// # Panics
///
/// Если `unit` не 1..=247 или `out.len()` не 1..=125.
pub async fn read_holding<T: Read + ReadReady + Write>(
    io: &mut T,
    unit: u8,
    start: u16,
    out: &mut [u16],
) -> Result<(), MasterError<T::Error>> {
    read_words(io, unit, 0x03, start, out).await
}

/// Читает input registers `start..start + out.len()` slave'а `unit` в `out`
/// (FC04).
///
/// # Errors
///
/// Отказ транспорта, исключение slave'а, ответ не на этот запрос.
///
/// # Panics
///
/// Если `unit` не 1..=247 или `out.len()` не 1..=125.
pub async fn read_input<T: Read + ReadReady + Write>(
    io: &mut T,
    unit: u8,
    start: u16,
    out: &mut [u16],
) -> Result<(), MasterError<T::Error>> {
    read_words(io, unit, 0x04, start, out).await
}

/// Пишет один coil (FC05). `unit` 0 — широковещательно, без ответа.
///
/// # Errors
///
/// Отказ транспорта, исключение slave'а, эхо не совпало с запросом.
///
/// # Panics
///
/// Если `unit` больше 247.
pub async fn write_coil<T: Read + ReadReady + Write>(
    io: &mut T,
    unit: u8,
    address: u16,
    value: bool,
) -> Result<(), MasterError<T::Error>> {
    let wire: u16 = if value { 0xFF00 } else { 0x0000 };
    write_single(io, unit, 0x05, address, wire).await
}

/// Пишет один holding register (FC06). `unit` 0 — широковещательно, без
/// ответа.
///
/// # Errors
///
/// Отказ транспорта, исключение slave'а, эхо не совпало с запросом.
///
/// # Panics
///
/// Если `unit` больше 247.
pub async fn write_register<T: Read + ReadReady + Write>(
    io: &mut T,
    unit: u8,
    address: u16,
    value: u16,
) -> Result<(), MasterError<T::Error>> {
    write_single(io, unit, 0x06, address, value).await
}

/// Пишет coils начиная со `start` (FC15). `unit` 0 — широковещательно, без
/// ответа.
///
/// # Errors
///
/// Отказ транспорта, исключение slave'а, ответ не на этот запрос.
///
/// # Panics
///
/// Если `unit` больше 247 или `values.len()` не 1..=1968.
pub async fn write_coils<T: Read + ReadReady + Write>(
    io: &mut T,
    unit: u8,
    start: u16,
    values: &[bool],
) -> Result<(), MasterError<T::Error>> {
    let qty = quantity(values.len(), MAX_WRITE_COILS);
    let bytes = usize::from(qty.div_ceil(8));
    let mut tx = [0_u8; MAX_ADU];
    multiple_head(&mut tx, unit, 0x0F, start, qty, bytes);
    for (i, &value) in values.iter().enumerate() {
        if value {
            tx[MULTIPLE_HEAD + i / 8] |= 1 << (i % 8);
        }
    }
    write_multiple(io, unit, 0x0F, start, qty, &mut tx, MULTIPLE_HEAD + bytes).await
}

/// Пишет holding registers начиная со `start` (FC16). `unit` 0 —
/// широковещательно, без ответа.
///
/// # Errors
///
/// Отказ транспорта, исключение slave'а, ответ не на этот запрос.
///
/// # Panics
///
/// Если `unit` больше 247 или `values.len()` не 1..=123.
pub async fn write_registers<T: Read + ReadReady + Write>(
    io: &mut T,
    unit: u8,
    start: u16,
    values: &[u16],
) -> Result<(), MasterError<T::Error>> {
    let qty = quantity(values.len(), MAX_WRITE_REGISTERS);
    let bytes = values.len() * 2;
    let mut tx = [0_u8; MAX_ADU];
    multiple_head(&mut tx, unit, 0x10, start, qty, bytes);
    for (slot, value) in tx[MULTIPLE_HEAD..MULTIPLE_HEAD + bytes]
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(values)
    {
        *slot = value.to_be_bytes();
    }
    write_multiple(io, unit, 0x10, start, qty, &mut tx, MULTIPLE_HEAD + bytes).await
}

async fn read_bits<T: Read + ReadReady + Write>(
    io: &mut T,
    unit: u8,
    fc: u8,
    start: u16,
    out: &mut [bool],
    max: u16,
) -> Result<(), MasterError<T::Error>> {
    let qty = quantity(out.len(), max);
    let mut rx = [0_u8; MAX_ADU];
    let len = read_request(io, unit, fc, start, qty, &mut rx).await?;
    let status = match decode(&rx[..len], fc)? {
        ResponsePdu::ReadCoils(r) if fc == 0x01 => r.coil_status,
        ResponsePdu::ReadDiscreteInputs(r) if fc == 0x02 => r.coil_status,
        _ => return Err(MasterError::Unexpected),
    };
    if status.len() != usize::from(qty.div_ceil(8)) {
        return Err(MasterError::Unexpected);
    }
    for (slot, value) in out.iter_mut().zip(Bits::new(status, qty).iter()) {
        *slot = value;
    }
    Ok(())
}

async fn read_words<T: Read + ReadReady + Write>(
    io: &mut T,
    unit: u8,
    fc: u8,
    start: u16,
    out: &mut [u16],
) -> Result<(), MasterError<T::Error>> {
    let qty = quantity(out.len(), MAX_READ_REGISTERS);
    let mut rx = [0_u8; MAX_ADU];
    let len = read_request(io, unit, fc, start, qty, &mut rx).await?;
    let data = match decode(&rx[..len], fc)? {
        ResponsePdu::ReadHoldingRegisters(r) if fc == 0x03 => r.register_data,
        ResponsePdu::ReadInputRegisters(r) if fc == 0x04 => r.register_data,
        _ => return Err(MasterError::Unexpected),
    };
    if data.len() != out.len() * 2 {
        return Err(MasterError::Unexpected);
    }
    for (slot, value) in out.iter_mut().zip(Words::new(data).iter()) {
        *slot = value;
    }
    Ok(())
}

/// Запрос чтения `qty` элементов: отправляет и принимает ответ в `rx`;
/// возвращает длину кадра ответа.
async fn read_request<T: Read + ReadReady + Write>(
    io: &mut T,
    unit: u8,
    fc: u8,
    start: u16,
    qty: u16,
    rx: &mut [u8; MAX_ADU],
) -> Result<usize, MasterError<T::Error>> {
    assert!(
        (1..=247).contains(&unit),
        "чтение с адреса {unit}: у slave'а адрес 1..=247, широковещательного чтения нет"
    );
    let mut tx = [0_u8; FIXED_REQUEST];
    head(&mut tx, unit, fc, start, qty);
    send(io, &mut tx, HEAD).await?;
    receive(io, unit, rx).await
}

async fn write_single<T: Read + ReadReady + Write>(
    io: &mut T,
    unit: u8,
    fc: u8,
    address: u16,
    value: u16,
) -> Result<(), MasterError<T::Error>> {
    assert!(unit <= 247, "запись на адрес {unit}: допустимы 0..=247");
    let mut tx = [0_u8; FIXED_REQUEST];
    head(&mut tx, unit, fc, address, value);
    send(io, &mut tx, HEAD).await?;
    if unit == 0 {
        return Ok(());
    }
    let mut rx = [0_u8; MAX_ADU];
    let len = receive(io, unit, &mut rx).await?;
    let echoed = match decode(&rx[..len], fc)? {
        ResponsePdu::WriteSingleCoil(r) if fc == 0x05 => {
            (r.address.0, if r.value.as_bool() { 0xFF00 } else { 0 })
        }
        ResponsePdu::WriteSingleRegister(r) if fc == 0x06 => (r.address.0, r.value),
        _ => return Err(MasterError::Unexpected),
    };
    if echoed == (address, value) {
        Ok(())
    } else {
        Err(MasterError::Unexpected)
    }
}

/// Длина заголовка любого запроса: адрес, функция, два 16-битных поля.
const HEAD: usize = 6;

/// Длина заголовка запроса FC15/16: [`HEAD`] и счётчик байт данных.
const MULTIPLE_HEAD: usize = HEAD + 1;

/// Пишет в `tx` адрес, функцию и два 16-битных поля запроса (адрес и
/// количество или значение).
fn head(tx: &mut [u8], unit: u8, fc: u8, first: u16, second: u16) {
    tx[..2].copy_from_slice(&[unit, fc]);
    tx[2..4].copy_from_slice(&first.to_be_bytes());
    tx[4..HEAD].copy_from_slice(&second.to_be_bytes());
}

/// Заголовок запроса FC15/16 в `tx`; данные идут с [`MULTIPLE_HEAD`].
fn multiple_head(tx: &mut [u8], unit: u8, fc: u8, start: u16, qty: u16, bytes: usize) {
    assert!(unit <= 247, "запись на адрес {unit}: допустимы 0..=247");
    head(tx, unit, fc, start, qty);
    // Не больше 246 байт: `quantity` уже ограничил количество.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "bytes ≤ 246 после quantity()"
    )]
    let count = bytes as u8;
    tx[HEAD] = count;
}

async fn write_multiple<T: Read + ReadReady + Write>(
    io: &mut T,
    unit: u8,
    fc: u8,
    start: u16,
    qty: u16,
    tx: &mut [u8; MAX_ADU],
    len: usize,
) -> Result<(), MasterError<T::Error>> {
    send(io, tx, len).await?;
    if unit == 0 {
        return Ok(());
    }
    let mut rx = [0_u8; MAX_ADU];
    let len = receive(io, unit, &mut rx).await?;
    let echoed = match decode(&rx[..len], fc)? {
        ResponsePdu::WriteMultipleCoils(r) if fc == 0x0F => (r.address.0, r.quantity.0),
        ResponsePdu::WriteMultipleRegisters(r) if fc == 0x10 => (r.address.0, r.quantity.0),
        _ => return Err(MasterError::Unexpected),
    };
    if echoed == (start, qty) {
        Ok(())
    } else {
        Err(MasterError::Unexpected)
    }
}

/// Количество элементов запроса, проверенное по спецификации.
fn quantity(len: usize, max: u16) -> u16 {
    match u16::try_from(len) {
        Ok(qty) if (1..=max).contains(&qty) => qty,
        _ => panic!("{len} элементов в одном запросе: допустимо 1..={max}"),
    }
}

/// Сливает входной буфер, приписывает CRC к первым `len` байтам `tx` и
/// отправляет кадр.
async fn send<T: Read + ReadReady + Write>(
    io: &mut T,
    tx: &mut [u8],
    len: usize,
) -> Result<(), MasterError<T::Error>> {
    drain(io).await?;
    let total = rtu::seal(tx, len);
    io.write_all(&tx[..total]).await.map_err(MasterError::Io)?;
    io.flush().await.map_err(MasterError::Io)
}

/// Выбрасывает то, что уже принято: опоздавший ответ на брошенный запрос
/// или хвост неразобранного кадра. Иначе ответ на этот запрос читался бы
/// со сдвигом, а опоздавший ответ на чтение был бы принят за свой — с
/// верной CRC и чужими данными.
async fn drain<T: Read + ReadReady>(io: &mut T) -> Result<(), MasterError<T::Error>> {
    let mut scratch = [0_u8; 32];
    let mut drained = 0;
    while drained < MAX_DRAIN && io.read_ready().map_err(MasterError::Io)? {
        let n = io.read(&mut scratch).await.map_err(MasterError::Io)?;
        if n == 0 {
            // Конец потока: дальше сливать нечего, а заметит его приём ответа.
            break;
        }
        drained += n;
    }
    Ok(())
}

/// Принимает один кадр ответа от `unit`; возвращает его длину.
async fn receive<T: Read>(
    io: &mut T,
    unit: u8,
    rx: &mut [u8; MAX_ADU],
) -> Result<usize, MasterError<T::Error>> {
    read(io, &mut rx[..2]).await?;
    let header = rtu::response_header(rx[1]).ok_or(MasterError::Unexpected)?;
    read(io, &mut rx[2..header]).await?;
    let len = rtu::response_len(&rx[..header]);
    if len > MAX_ADU {
        return Err(MasterError::Unexpected);
    }
    read(io, &mut rx[header..len]).await?;
    if !rtu::crc_ok(&rx[..len]) {
        return Err(MasterError::Crc);
    }
    if rx[0] != unit {
        return Err(MasterError::Unexpected);
    }
    Ok(len)
}

async fn read<T: Read>(io: &mut T, buf: &mut [u8]) -> Result<(), MasterError<T::Error>> {
    io.read_exact(buf).await.map_err(|e| match e {
        ReadExactError::UnexpectedEof => MasterError::Eof,
        ReadExactError::Other(e) => MasterError::Io(e),
    })
}

/// Разбирает PDU кадра ответа на функцию `fc`; исключение на неё — ошибка.
fn decode<E>(frame: &[u8], fc: u8) -> Result<ResponsePdu<'_>, MasterError<E>> {
    match decode_response(&frame[1..frame.len() - 2]) {
        Ok(ResponsePdu::Exception(e)) if e.function_code.code() == fc => Err(
            MasterError::Exception(Exception::from_code(e.exception_code.code())),
        ),
        Ok(ResponsePdu::Exception(_)) | Err(_) => Err(MasterError::Unexpected),
        Ok(pdu) => Ok(pdu),
    }
}

#[cfg(test)]
mod tests {
    use core::convert::Infallible;

    use super::*;
    use crate::modbus::fakes::{Duplex, Map, frame};
    use crate::modbus::slave::serve;
    use ports::ServeError;

    /// Запрос master'а → наш slave → ответ обратно master'у: две реализации
    /// проверяют друг друга, и ни одна не знает, как устроена другая.
    async fn through_slave<F, Fut>(map: &mut Map, call: F) -> Result<(), MasterError<Infallible>>
    where
        F: Fn(Duplex) -> Fut,
        Fut: Future<Output = (Result<(), MasterError<Infallible>>, Vec<u8>)>,
    {
        // Первый прогон: ответа нет, master пишет запрос и упирается в конец потока.
        let (first, request) = call(Duplex::replying(&[])).await;
        assert_eq!(first, Err(MasterError::Eof));
        let mut slave_io = Duplex::new(&request);
        assert_eq!(serve(&mut slave_io, 1, map).await, Err(ServeError::Eof));
        call(Duplex::replying(&slave_io.tx)).await.0
    }

    #[tokio::test]
    async fn holding_roundtrip_through_slave() {
        let mut map = Map::default();
        map.holding[5..8].copy_from_slice(&[1, 0xBEEF, 3]);
        let got = std::cell::RefCell::new([0_u16; 3]);
        let result = through_slave(&mut map, |mut io| {
            let got = &got;
            async move {
                let mut out = [0; 3];
                let r = read_holding(&mut io, 1, 5, &mut out).await;
                *got.borrow_mut() = out;
                (r, io.tx)
            }
        })
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(*got.borrow(), [1, 0xBEEF, 3]);
    }

    #[tokio::test]
    async fn write_registers_roundtrip_through_slave() {
        let mut map = Map::default();
        let result = through_slave(&mut map, |mut io| async move {
            let r = write_registers(&mut io, 1, 2, &[10, 20]).await;
            (r, io.tx)
        })
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(map.holding[2..4], [10, 20]);
    }

    #[tokio::test]
    async fn coils_roundtrip_through_slave() {
        let mut map = Map::default();
        let values = [true, false, true, true, false, false, false, false, true];
        let result = through_slave(&mut map, |mut io| async move {
            let r = write_coils(&mut io, 1, 3, &values).await;
            (r, io.tx)
        })
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(map.coils[3..12], values);
    }

    #[tokio::test]
    async fn request_bytes_are_spec_example() {
        // Пример из спецификации: прочитать 3 регистра с 0x006B у slave'а 17.
        let mut io = Duplex::replying(&[]);
        let _ignored = read_holding(&mut io, 0x11, 0x6B, &mut [0; 3]).await;
        assert_eq!(io.tx, frame(0x11, &[0x03, 0x00, 0x6B, 0x00, 0x03]));
    }

    #[tokio::test]
    async fn stale_reply_is_drained_not_taken() {
        // Опоздавший ответ на брошенный запрос уже лежит в буфере; свой
        // приходит после отправки — его master и должен вернуть.
        let stale = frame(1, &[0x03, 2, 0, 1]);
        let fresh = frame(1, &[0x03, 2, 0, 2]);
        let mut io = Duplex::replying(&fresh).with_stale(&stale);
        let mut out = [0];
        assert_eq!(read_holding(&mut io, 1, 100, &mut out).await, Ok(()));
        assert_eq!(out, [2]);
    }

    #[tokio::test]
    async fn exception_is_reported() {
        for (code, exception) in [
            (0x02, Exception::IllegalDataAddress),
            (0x0B, Exception::GatewayTargetFailed),
        ] {
            let reply = frame(1, &[0x83, code]);
            let mut io = Duplex::replying(&reply);
            let r = read_holding(&mut io, 1, 0, &mut [0; 1]).await;
            assert_eq!(r, Err(MasterError::Exception(exception)));
        }
    }

    #[tokio::test]
    async fn mismatched_responses_are_unexpected() {
        let cases: [Vec<u8>; 6] = [
            // просили 3 регистра — пришёл 1
            frame(1, &[0x03, 2, 0, 7]),
            // пустые данные
            frame(1, &[0x03, 0]),
            // ответ другого slave'а
            frame(2, &[0x03, 6, 0, 1, 0, 2, 0, 3]),
            // ответ на другую функцию
            frame(1, &[0x04, 6, 0, 1, 0, 2, 0, 3]),
            // исключение на другую функцию
            frame(1, &[0x84, 0x02]),
            // функция, длину ответа которой не вывести
            frame(1, &[0x2B, 0x0E]),
        ];
        for reply in cases {
            let mut io = Duplex::replying(&reply);
            let r = read_holding(&mut io, 1, 0, &mut [0; 3]).await;
            assert_eq!(r, Err(MasterError::Unexpected), "ответ {reply:02X?}");
        }
    }

    #[tokio::test]
    async fn bad_crc_and_short_stream() {
        let mut reply = frame(1, &[0x03, 2, 0, 7]);
        reply[3] ^= 1;
        let mut io = Duplex::replying(&reply);
        let r = read_holding(&mut io, 1, 0, &mut [0; 1]).await;
        assert_eq!(r, Err(MasterError::Crc));
        let mut io = Duplex::replying(&reply[..4]);
        let r = read_holding(&mut io, 1, 0, &mut [0; 1]).await;
        assert_eq!(r, Err(MasterError::Eof));
    }

    #[tokio::test]
    async fn broadcast_write_does_not_wait() {
        let mut io = Duplex::replying(&[]);
        assert_eq!(write_register(&mut io, 0, 1, 2).await, Ok(()));
        assert_eq!(io.tx, frame(0, &[0x06, 0, 1, 0, 2]));
    }

    #[tokio::test]
    #[should_panic(expected = "126 элементов")]
    async fn too_many_registers_is_a_bug() {
        let mut io = Duplex::replying(&[]);
        let _ignored = read_holding(&mut io, 1, 0, &mut [0; 126]).await;
    }

    proptest::proptest! {
        /// Какой бы ответ ни пришёл — master не паникует.
        #[test]
        fn never_panics(reply in proptest::collection::vec(proptest::num::u8::ANY, 0..300)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let mut io = Duplex::replying(&reply);
                let _ignored = read_holding(&mut io, 1, 0, &mut [0; 4]).await;
                let mut io = Duplex::replying(&reply);
                let _ignored = read_coils(&mut io, 1, 0, &mut [false; 20]).await;
                let mut io = Duplex::replying(&reply);
                let _ignored = write_coils(&mut io, 1, 0, &[true; 3]).await;
            });
        }

        /// Ответ с верной CRC и согласованной длиной, но случайным
        /// содержимым, доходит до разбора и сверки с запросом — и там тоже
        /// без паники.
        #[test]
        fn never_panics_on_valid_crc(
            fc in proptest::sample::select(vec![0x01_u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x0F, 0x10, 0x83]),
            data in proptest::collection::vec(proptest::num::u8::ANY, 0..60),
        ) {
            // Длина PDU такая, какую master выведет из заголовка, — иначе
            // кадр отсекался бы ещё до разбора.
            let pdu: Vec<u8> = match fc {
                0x01..=0x04 => {
                    let count = u8::try_from(data.len()).unwrap();
                    [&[fc, count][..], &data].concat()
                }
                0x83 => vec![fc, data.first().copied().unwrap_or(0)],
                _ => [&[fc][..], &data, &[0; 4]].concat()[..5].to_vec(),
            };
            let reply = frame(1, &pdu);
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let mut io = Duplex::replying(&reply);
                let _ignored = read_holding(&mut io, 1, 0, &mut [0; 4]).await;
                let mut io = Duplex::replying(&reply);
                let _ignored = read_discrete_inputs(&mut io, 1, 0, &mut [false; 20]).await;
                let mut io = Duplex::replying(&reply);
                let _ignored = write_register(&mut io, 1, 0, 1).await;
                let mut io = Duplex::replying(&reply);
                let _ignored = write_coils(&mut io, 1, 0, &[true; 3]).await;
            });
        }
    }
}
