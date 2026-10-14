//! Modbus RTU slave поверх потока байтов.

use core::convert::Infallible;

use embedded_io_async::{Read, ReadExactError, Write};
use ports::{Bits, BitsMut, Exception, Registers, ServeError, Words, WordsMut};
use rusty_modbus_codec::error::DecodeError;
use rusty_modbus_codec::validate::{
    validate_read_coils, validate_read_discrete_inputs, validate_read_registers,
    validate_write_coils, validate_write_registers,
};
use rusty_modbus_codec::{RequestPdu, decode_request};
use rusty_modbus_types::ExceptionCode;

use super::rtu::{self, MAX_ADU};

/// Обслуживает линию как slave с адресом `unit`: принимает запросы, отдаёт
/// их `regs` и отвечает — до отказа транспорта.
///
/// Чего slave не делает и почему:
///
/// * не отвечает на запрос с чужим адресом и на широковещательный (адрес 0)
///   — запись из широковещательного при этом выполняется, чтение нет;
/// * не отвечает на кадр с неверной CRC: границу кадра в потоке он ищет по
///   содержимому, и после такого кадра сдвигается на байт, пока CRC не
///   сойдётся снова;
/// * не отвечает на функции, кроме FC01–06, 15, 16: длину их кадра по коду
///   функции не вывести, а отвечать, не проверив CRC, нельзя — master
///   получит таймаут, а не Illegal Function.
///
/// Недопустимый запрос — количество вне диапазона спецификации, счётчик
/// байт не сходится, coil не 0x0000/0xFF00 — получает исключение до вызова
/// `regs`.
///
/// # Errors
///
/// Возвращает отказ транспорта или конец потока; ошибкой не кончается
/// ничего, что приходит по линии.
///
/// # Panics
///
/// Если `unit` не 1..=247 — адреса slave'а по спецификации.
pub async fn serve<T, R>(
    io: &mut T,
    unit: u8,
    regs: &mut R,
) -> Result<Infallible, ServeError<T::Error>>
where
    T: Read + Write,
    R: Registers,
{
    assert!(
        (1..=247).contains(&unit),
        "адрес slave'а {unit} вне 1..=247"
    );
    let mut rx = [0_u8; MAX_ADU];
    let mut have = 0;
    let mut tx = [0_u8; MAX_ADU];
    loop {
        let len = next_request(io, &mut rx, &mut have).await?;
        let addr = rx[0];
        let broadcast = addr == 0;
        // Широковещательное чтение не выполняется вовсе: ответа на него нет,
        // а у чтения бывают побочные эффекты (сброс счётчика при чтении).
        let read = matches!(rx[1], 0x01..=0x04);
        if addr == unit || (broadcast && !read) {
            let reply = respond(&rx[1..len - 2], regs, &mut tx[1..]).await;
            if !broadcast {
                tx[0] = unit;
                let total = rtu::seal(&mut tx, 1 + reply);
                io.write_all(&tx[..total]).await.map_err(ServeError::Io)?;
                io.flush().await.map_err(ServeError::Io)?;
            }
        }
        consume(&mut rx, &mut have, len);
    }
}

/// Ждёт кадр запроса с верной CRC в начале `rx`; возвращает его длину.
///
/// `rx[..*have]` — принятые, но ещё не разобранные байты. Читает ровно
/// столько, сколько не хватает до кадра, — ни байтом больше, иначе начало
/// следующего кадра пришлось бы хранить где-то ещё.
async fn next_request<T: Read>(
    io: &mut T,
    rx: &mut [u8; MAX_ADU],
    have: &mut usize,
) -> Result<usize, ServeError<T::Error>> {
    loop {
        fill(io, rx, have, 2).await?;
        let Some(header) = rtu::request_header(rx[1]) else {
            consume(rx, have, 1);
            continue;
        };
        fill(io, rx, have, header).await?;
        let len = rtu::request_len(&rx[..header]);
        if len <= MAX_ADU {
            fill(io, rx, have, len).await?;
            if rtu::crc_ok(&rx[..len]) {
                return Ok(len);
            }
        }
        consume(rx, have, 1);
    }
}

/// Дочитывает `rx` до `want` байт.
async fn fill<T: Read>(
    io: &mut T,
    rx: &mut [u8; MAX_ADU],
    have: &mut usize,
    want: usize,
) -> Result<(), ServeError<T::Error>> {
    if *have < want {
        io.read_exact(&mut rx[*have..want])
            .await
            .map_err(|e| match e {
                ReadExactError::UnexpectedEof => ServeError::Eof,
                ReadExactError::Other(e) => ServeError::Io(e),
            })?;
        *have = want;
    }
    Ok(())
}

/// Убирает `n` байт из начала принятого.
fn consume(rx: &mut [u8; MAX_ADU], have: &mut usize, n: usize) {
    rx.copy_within(n..*have, 0);
    *have -= n;
}

/// Выполняет запрос `pdu` и кладёт PDU ответа в `out`; возвращает его длину.
async fn respond<R: Registers>(pdu: &[u8], regs: &mut R, out: &mut [u8]) -> usize {
    let fc = pdu[0];
    // Сейчас сюда доходят только FC01–06/15/16 (`rtu::request_header`), и
    // неизвестной функции не бывает; ветка — для транспорта, где границу
    // кадра задаёт линия (`ports::FrameLink`), и кадр с любой функцией
    // доходит до разбора.
    let result = match decode_request(pdu) {
        Ok(request) => execute(&request, pdu, regs, out).await,
        Err(DecodeError::UnknownFunctionCode(_)) => Err(Exception::IllegalFunction),
        Err(_) => Err(Exception::IllegalDataValue),
    };
    match result {
        Ok(len) => len,
        Err(exception) => {
            out[0] = fc | 0x80;
            out[1] = exception.code();
            2
        }
    }
}

async fn execute<R: Registers>(
    request: &RequestPdu<'_>,
    pdu: &[u8],
    regs: &mut R,
    out: &mut [u8],
) -> Result<usize, Exception> {
    match request {
        RequestPdu::ReadCoils(r) => {
            let (start, qty) = (r.address.0, r.quantity.0);
            validate_read_coils(start, qty).map_err(exception)?;
            let n = bits_reply(out, pdu[0], qty);
            regs.read_coils(start, BitsMut::new(&mut out[2..n], qty))
                .await?;
            Ok(n)
        }
        RequestPdu::ReadDiscreteInputs(r) => {
            let (start, qty) = (r.address.0, r.quantity.0);
            validate_read_discrete_inputs(start, qty).map_err(exception)?;
            let n = bits_reply(out, pdu[0], qty);
            regs.read_discrete_inputs(start, BitsMut::new(&mut out[2..n], qty))
                .await?;
            Ok(n)
        }
        RequestPdu::ReadHoldingRegisters(r) => {
            let (start, qty) = (r.address.0, r.quantity.0);
            validate_read_registers(start, qty).map_err(exception)?;
            let n = words_reply(out, pdu[0], qty);
            regs.read_holding(start, WordsMut::new(&mut out[2..n]))
                .await?;
            Ok(n)
        }
        RequestPdu::ReadInputRegisters(r) => {
            let (start, qty) = (r.address.0, r.quantity.0);
            validate_read_registers(start, qty).map_err(exception)?;
            let n = words_reply(out, pdu[0], qty);
            regs.read_input(start, WordsMut::new(&mut out[2..n]))
                .await?;
            Ok(n)
        }
        RequestPdu::WriteSingleCoil(r) => {
            let bit = [u8::from(r.value.as_bool())];
            regs.write_coils(r.address.0, Bits::new(&bit, 1)).await?;
            Ok(echo(out, pdu, pdu.len()))
        }
        RequestPdu::WriteSingleRegister(r) => {
            let word = r.value.to_be_bytes();
            regs.write_holding(r.address.0, Words::new(&word)).await?;
            Ok(echo(out, pdu, pdu.len()))
        }
        RequestPdu::WriteMultipleCoils(r) => {
            let (start, qty) = (r.address.0, r.quantity.0);
            validate_write_coils(start, qty, r.byte_count).map_err(exception)?;
            regs.write_coils(start, Bits::new(r.coil_values, qty))
                .await?;
            Ok(echo(out, pdu, ECHO_MULTIPLE))
        }
        RequestPdu::WriteMultipleRegisters(r) => {
            let (start, qty) = (r.address.0, r.quantity.0);
            validate_write_registers(start, qty, r.byte_count).map_err(exception)?;
            regs.write_holding(start, Words::new(r.register_values))
                .await?;
            Ok(echo(out, pdu, ECHO_MULTIPLE))
        }
        _ => Err(Exception::IllegalFunction),
    }
}

/// Ответ на FC15/16 — код функции, адрес и количество из запроса.
const ECHO_MULTIPLE: usize = 5;

/// Заголовок ответа на чтение `qty` бит; возвращает длину PDU ответа.
fn bits_reply(out: &mut [u8], fc: u8, qty: u16) -> usize {
    reply_head(out, fc, usize::from(qty.div_ceil(8)))
}

/// Заголовок ответа на чтение `qty` регистров; возвращает длину PDU ответа.
fn words_reply(out: &mut [u8], fc: u8, qty: u16) -> usize {
    reply_head(out, fc, usize::from(qty) * 2)
}

fn reply_head(out: &mut [u8], fc: u8, bytes: usize) -> usize {
    out[0] = fc;
    // Не больше 250 байт: количество уже проверено `validate_*`.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "count ≤ 250 после validate_*"
    )]
    let count = bytes as u8;
    out[1] = count;
    2 + bytes
}

/// Копирует первые `len` байт запроса в ответ.
fn echo(out: &mut [u8], pdu: &[u8], len: usize) -> usize {
    out[..len].copy_from_slice(&pdu[..len]);
    len
}

fn exception(code: ExceptionCode) -> Exception {
    Exception::from_code(code.code())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modbus::fakes::{Duplex, Map, frame};

    /// Прогоняет `input` через slave с адресом 1; возвращает ответы целиком.
    async fn run(input: &[u8], map: &mut Map) -> Vec<u8> {
        let mut io = Duplex::new(input);
        let end = serve(&mut io, 1, map).await;
        assert_eq!(end, Err(ServeError::Eof), "slave кончился не концом потока");
        io.tx
    }

    #[tokio::test]
    async fn reads_holding_registers() {
        let mut map = Map::default();
        map.holding[2] = 0x1234;
        map.holding[3] = 0xABCD;
        let out = run(&frame(1, &[0x03, 0, 2, 0, 2]), &mut map).await;
        assert_eq!(out, frame(1, &[0x03, 4, 0x12, 0x34, 0xAB, 0xCD]));
    }

    #[tokio::test]
    async fn reads_coils_packed_lsb_first() {
        let mut map = Map::default();
        map.coils[0] = true;
        map.coils[2] = true;
        map.coils[8] = true;
        let out = run(&frame(1, &[0x01, 0, 0, 0, 9]), &mut map).await;
        assert_eq!(out, frame(1, &[0x01, 2, 0b0000_0101, 0b0000_0001]));
    }

    #[tokio::test]
    async fn write_multiple_registers_arrives_in_one_call() {
        let mut map = Map::default();
        let out = run(&frame(1, &[0x10, 0, 4, 0, 2, 4, 0, 1, 0, 2]), &mut map).await;
        assert_eq!(out, frame(1, &[0x10, 0, 4, 0, 2]));
        assert_eq!(map.writes, vec![(4, vec![1, 2])]);
    }

    #[tokio::test]
    async fn single_write_echoes_request() {
        let mut map = Map::default();
        let coil = frame(1, &[0x05, 0, 3, 0xFF, 0x00]);
        let reg = frame(1, &[0x06, 0, 1, 0x00, 0x2A]);
        let out = run(&[coil.clone(), reg.clone()].concat(), &mut map).await;
        assert_eq!(out, [coil, reg].concat());
        assert!(map.coils[3]);
        assert_eq!(map.holding[1], 42);
    }

    #[tokio::test]
    async fn invalid_requests_get_exceptions() {
        let cases: [(&[u8], &[u8]); 5] = [
            // qty=0 и qty>125 — Illegal Data Value
            (&[0x03, 0, 0, 0, 0], &[0x83, 0x03]),
            (&[0x03, 0, 0, 0, 200], &[0x83, 0x03]),
            // coil не 0x0000/0xFF00
            (&[0x05, 0, 0, 0x12, 0x34], &[0x85, 0x03]),
            // FC16: qty=3, а байт 4
            (&[0x10, 0, 0, 0, 3, 4, 0, 1, 0, 2], &[0x90, 0x03]),
            // вне карты устройства — отвечает само приложение
            (&[0x03, 0, 15, 0, 2], &[0x83, 0x02]),
        ];
        for (request, reply) in cases {
            let mut map = Map::default();
            let out = run(&frame(1, request), &mut map).await;
            assert_eq!(out, frame(1, reply), "запрос {request:02X?}");
            assert!(
                map.writes.is_empty(),
                "при исключении записано {:?}",
                map.writes
            );
        }
    }

    #[tokio::test]
    async fn unimplemented_table_is_illegal_function() {
        let mut map = Map::default();
        let out = run(&frame(1, &[0x04, 0, 0, 0, 1]), &mut map).await;
        assert_eq!(out, frame(1, &[0x84, 0x01]));
    }

    #[tokio::test]
    async fn foreign_and_broadcast_get_no_reply() {
        let mut map = Map::default();
        let foreign = frame(2, &[0x06, 0, 0, 0, 7]);
        let broadcast = frame(0, &[0x06, 0, 1, 0, 9]);
        let out = run(&[foreign, broadcast].concat(), &mut map).await;
        assert!(out.is_empty(), "ответ {out:02X?}");
        assert_eq!(map.holding[0], 0, "записал чужой запрос");
        assert_eq!(map.holding[1], 9, "не выполнил широковещательную запись");
    }

    #[tokio::test]
    async fn resyncs_after_garbage_and_bad_crc() {
        let mut map = Map::default();
        map.holding[0] = 5;
        let good = frame(1, &[0x03, 0, 0, 0, 1]);
        let mut broken = good.clone();
        broken[3] ^= 0xFF;
        let input = [&[0x55, 0x01, 0xAA][..], &broken, &good].concat();
        let out = run(&input, &mut map).await;
        assert_eq!(out, frame(1, &[0x03, 2, 0, 5]));
    }

    proptest::proptest! {
        /// Что бы ни пришло по линии — slave не паникует и кончается только
        /// концом потока.
        #[test]
        fn never_panics(input in proptest::collection::vec(proptest::num::u8::ANY, 0..600)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let mut map = Map::default();
                run(&input, &mut map).await;
            });
        }

        /// Кадр с верной CRC, согласованной длиной и случайным
        /// содержимым доходит до разбора и исполнения — и там тоже без
        /// паники; ответ — кадр с верной CRC.
        #[test]
        fn never_panics_on_valid_crc(
            fc in proptest::sample::select(vec![0x01_u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x0F, 0x10]),
            fields in proptest::array::uniform4(proptest::num::u8::ANY),
            data in proptest::collection::vec(proptest::num::u8::ANY, 0..40),
        ) {
            // Длина кадра такая, какую slave выведет из заголовка, а у
            // FC15/16 количество согласовано со счётчиком байт — иначе кадр
            // отсекался бы ещё до разбора или до `Bits::new`/`Words::new`.
            let pdu: Vec<u8> = match fc {
                0x0F | 0x10 => {
                    let mut data = data;
                    let qty = if fc == 0x0F {
                        (data.len() * 8).saturating_sub(usize::from(fields[3] % 8))
                    } else {
                        data.truncate(data.len() / 2 * 2);
                        data.len() / 2
                    };
                    let qty = u16::try_from(qty).unwrap().to_be_bytes();
                    let count = u8::try_from(data.len()).unwrap();
                    [&[fc, fields[0], fields[1]][..], &qty, &[count], &data].concat()
                }
                _ => [&[fc][..], &fields].concat(),
            };
            let request = frame(1, &pdu);
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let out = rt.block_on(async {
                let mut map = Map::default();
                run(&request, &mut map).await
            });
            proptest::prop_assert!(rtu::crc_ok(&out), "ответ {out:02X?}");
        }
    }
}
