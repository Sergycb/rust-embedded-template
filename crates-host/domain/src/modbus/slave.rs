//! Modbus slave — обработчик кадра «адрес и PDU» для узла протокола
//! `domain::server`.

use ports::{Bits, BitsMut, Exception, Handler, MAX_PDU, Registers, Words, WordsMut};
use rusty_modbus_codec::error::DecodeError;
use rusty_modbus_codec::validate::{
    validate_read_coils, validate_read_discrete_inputs, validate_read_registers,
    validate_write_coils, validate_write_registers,
};
use rusty_modbus_codec::{RequestPdu, decode_request};
use rusty_modbus_types::ExceptionCode;

/// Ответ на запись — первые пять байт запроса: функция, адрес и значение
/// (FC05/06) или количество (FC15/16).
const ECHO: usize = 5;

/// Slave с адресом `unit` над данными `regs`: кадр «адрес и PDU» из канала
/// (`adapters::modbus::{Rtu, Tcp, Udp}`) — в запрос к `regs`, ответ — в тот
/// же кадр.
///
/// Отвечает на запрос с адресом `unit` и с 0xFF — так TCP и UDP
/// обращаются к самому устройству. На чужой адрес и на широковещательный
/// (0) не отвечает; запись из широковещательного при этом выполняется,
/// чтение нет.
///
/// Недопустимый запрос — количество вне диапазона спецификации, счётчик
/// байт не сходится, coil не 0x0000/0xFF00 — получает исключение до вызова
/// `regs`, неизвестная функция — Illegal Function. Битые кадры отбрасывает
/// канал.
#[derive(Debug)]
pub struct Slave<R> {
    unit: u8,
    regs: R,
}

impl<R> Slave<R> {
    /// Slave с адресом `unit` над данными `regs`.
    ///
    /// # Panics
    ///
    /// Если `unit` не 1..=247 — адреса slave'а по спецификации.
    pub fn new(unit: u8, regs: R) -> Self {
        assert!(
            (1..=247).contains(&unit),
            "адрес slave'а {unit} вне 1..=247"
        );
        Self { unit, regs }
    }

    /// Данные обратно — тестам, которые смотрят карту после запросов.
    #[cfg(test)]
    pub(crate) fn into_regs(self) -> R {
        self.regs
    }
}

impl<R: Registers> Handler for Slave<R> {
    fn begin(&mut self, capacity: usize) {
        assert!(
            capacity > MAX_PDU,
            "буфер кадра {capacity} не вмещает адрес и PDU ({} байт)",
            MAX_PDU + 1
        );
        defmt_or_log::info!("modbus: slave, адрес {}", self.unit);
    }

    async fn handle(&mut self, frame: &mut [u8], len: usize) -> Option<usize> {
        let (&to, pdu) = frame[..len].split_first()?;
        let &fc = pdu.first()?;
        let answer = to == self.unit || to == 0xFF;
        // Широковещательное чтение не выполняется вовсе: ответа на него нет, а
        // у чтения бывают побочные эффекты (сброс счётчика при чтении).
        let broadcast_write = to == 0 && !matches!(fc, 0x01..=0x04);
        if !answer && !broadcast_write {
            return None;
        }
        // Ответ пишется поверх запроса, а запрос нужен до конца исполнения.
        let mut request = [0_u8; MAX_PDU];
        let request = &mut request[..pdu.len()];
        request.copy_from_slice(pdu);
        let reply = respond(request, &mut self.regs, &mut frame[1..]).await;
        answer.then_some(1 + reply)
    }
}

/// Выполняет запрос `pdu` и кладёт PDU ответа в `out`; возвращает его длину.
async fn respond<R: Registers>(pdu: &[u8], regs: &mut R, out: &mut [u8]) -> usize {
    let fc = pdu[0];
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
            Ok(echo(out, pdu))
        }
        RequestPdu::WriteSingleRegister(r) => {
            let word = r.value.to_be_bytes();
            regs.write_holding(r.address.0, Words::new(&word)).await?;
            Ok(echo(out, pdu))
        }
        RequestPdu::WriteMultipleCoils(r) => {
            let (start, qty) = (r.address.0, r.quantity.0);
            validate_write_coils(start, qty, r.byte_count).map_err(exception)?;
            regs.write_coils(start, Bits::new(r.coil_values, qty))
                .await?;
            Ok(echo(out, pdu))
        }
        RequestPdu::WriteMultipleRegisters(r) => {
            let (start, qty) = (r.address.0, r.quantity.0);
            validate_write_registers(start, qty, r.byte_count).map_err(exception)?;
            regs.write_holding(start, Words::new(r.register_values))
                .await?;
            Ok(echo(out, pdu))
        }
        _ => Err(Exception::IllegalFunction),
    }
}

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

/// Копирует в ответ первые [`ECHO`] байт запроса записи.
fn echo(out: &mut [u8], pdu: &[u8]) -> usize {
    out[..ECHO].copy_from_slice(&pdu[..ECHO]);
    ECHO
}

fn exception(code: ExceptionCode) -> Exception {
    Exception::from_code(code.code())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modbus::fakes::Map;

    /// Прогоняет запросы `(адрес, PDU)` через slave с адресом 1; возвращает
    /// ответы.
    async fn run(requests: &[(u8, &[u8])], map: &mut Map) -> Vec<Vec<u8>> {
        let mut slave = Slave::new(1, core::mem::take(map));
        let mut replies = Vec::new();
        for &(unit, pdu) in requests {
            let mut frame = [0; MAX_PDU + 1];
            frame[0] = unit;
            frame[1..=pdu.len()].copy_from_slice(pdu);
            if let Some(len) = slave.handle(&mut frame, 1 + pdu.len()).await {
                assert_eq!(frame[0], unit, "адрес ответа — адрес запроса");
                replies.push(frame[1..len].to_vec());
            }
        }
        *map = slave.into_regs();
        replies
    }

    #[tokio::test]
    async fn reads_holding_registers() {
        let mut map = Map::default();
        map.holding[2] = 0x1234;
        map.holding[3] = 0xABCD;
        let out = run(&[(1, &[0x03, 0, 2, 0, 2])], &mut map).await;
        assert_eq!(out, [[0x03, 4, 0x12, 0x34, 0xAB, 0xCD]]);
    }

    #[tokio::test]
    async fn reads_coils_packed_lsb_first() {
        let mut map = Map::default();
        map.coils[0] = true;
        map.coils[2] = true;
        map.coils[8] = true;
        let out = run(&[(1, &[0x01, 0, 0, 0, 9])], &mut map).await;
        assert_eq!(out, [[0x01, 2, 0b0000_0101, 0b0000_0001]]);
    }

    #[tokio::test]
    async fn write_multiple_registers_arrives_in_one_call() {
        let mut map = Map::default();
        let out = run(&[(1, &[0x10, 0, 4, 0, 2, 4, 0, 1, 0, 2])], &mut map).await;
        assert_eq!(out, [[0x10, 0, 4, 0, 2]]);
        assert_eq!(map.writes, vec![(4, vec![1, 2])]);
    }

    #[tokio::test]
    async fn single_write_echoes_request() {
        let mut map = Map::default();
        let coil = [0x05, 0, 3, 0xFF, 0x00];
        let reg = [0x06, 0, 1, 0x00, 0x2A];
        let out = run(&[(1, &coil), (1, &reg)], &mut map).await;
        assert_eq!(out, [coil, reg]);
        assert!(map.coils[3]);
        assert_eq!(map.holding[1], 42);
    }

    #[tokio::test]
    async fn invalid_requests_get_exceptions() {
        let cases: [(&[u8], &[u8]); 7] = [
            // qty=0 и qty>125 — Illegal Data Value
            (&[0x03, 0, 0, 0, 0], &[0x83, 0x03]),
            (&[0x03, 0, 0, 0, 200], &[0x83, 0x03]),
            // coil не 0x0000/0xFF00
            (&[0x05, 0, 0, 0x12, 0x34], &[0x85, 0x03]),
            // FC16: qty=3, а байт 4
            (&[0x10, 0, 0, 0, 3, 4, 0, 1, 0, 2], &[0x90, 0x03]),
            // вне карты устройства — отвечает само приложение
            (&[0x03, 0, 15, 0, 2], &[0x83, 0x02]),
            // функция, которой slave не знает, — TCP и UDP доносят её
            (&[0x2B, 0x0E, 0x01, 0x00], &[0xAB, 0x01]),
            // запрос обрезан
            (&[0x03, 0, 0], &[0x83, 0x03]),
        ];
        for (request, reply) in cases {
            let mut map = Map::default();
            let out = run(&[(1, request)], &mut map).await;
            assert_eq!(out, [reply], "запрос {request:02X?}");
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
        let out = run(&[(1, &[0x04, 0, 0, 0, 1])], &mut map).await;
        assert_eq!(out, [[0x84, 0x01]]);
    }

    #[tokio::test]
    async fn addressing() {
        let mut map = Map::default();
        let out = run(
            &[
                (2, &[0x06, 0, 0, 0, 7]),
                (0, &[0x06, 0, 1, 0, 9]),
                (0xFF, &[0x06, 0, 2, 0, 3]),
                (1, &[]),
            ],
            &mut map,
        )
        .await;
        assert_eq!(out, [[0x06, 0, 2, 0, 3]], "ответ только на 0xFF");
        assert_eq!(map.holding[0], 0, "записал чужой запрос");
        assert_eq!(map.holding[1], 9, "не выполнил широковещательную запись");
        assert_eq!(map.holding[2], 3);
    }

    /// Широковещательное чтение не выполняется вовсе и не отвечает: у чтения
    /// бывают побочные эффекты.
    #[tokio::test]
    async fn broadcast_reads_are_neither_executed_nor_answered() {
        let mut map = Map::default();
        let out = run(
            &[
                (0, &[0x01, 0, 0, 0, 1]),
                (0, &[0x02, 0, 0, 0, 1]),
                (0, &[0x03, 0, 0, 0, 1]),
                (0, &[0x04, 0, 0, 0, 1]),
            ],
            &mut map,
        )
        .await;
        assert!(out.is_empty(), "ответов быть не должно: {out:02X?}");
        assert_eq!(map.reads, 0, "карту не читали");
    }

    /// Пустая карта шаблона на любой запрос отвечает Illegal Function.
    #[tokio::test]
    async fn the_empty_map_answers_illegal_function() {
        let mut slave = Slave::new(1, crate::modbus::EmptyRegisters);
        let mut replies = Vec::new();
        for pdu in [
            &[0x01, 0, 0, 0, 1][..],
            &[0x02, 0, 0, 0, 1],
            &[0x03, 0, 0, 0, 1],
            &[0x04, 0, 0, 0, 1],
            &[0x05, 0, 0, 0xFF, 0x00],
            &[0x06, 0, 0, 0, 1],
        ] {
            let mut frame = [0; MAX_PDU + 1];
            frame[0] = 1;
            frame[1..=pdu.len()].copy_from_slice(pdu);
            let len = slave
                .handle(&mut frame, 1 + pdu.len())
                .await
                .expect("ответ");
            replies.push(frame[1..len].to_vec());
        }
        let expected: Vec<Vec<u8>> = (1..=6_u8).map(|fc| vec![fc | 0x80, 0x01]).collect();
        assert_eq!(replies, expected);
    }

    proptest::proptest! {
        /// Какой бы PDU ни пришёл — slave не паникует и отвечает на него.
        #[test]
        fn never_panics(pdu in proptest::collection::vec(proptest::num::u8::ANY, 1..=MAX_PDU)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let out = rt.block_on(async {
                let mut map = Map::default();
                run(&[(1, &pdu)], &mut map).await
            });
            proptest::prop_assert_eq!(out.len(), 1);
        }

        /// PDU записи с согласованным количеством и случайным содержимым
        /// доходит до `Bits::new`/`Words::new` и исполнения — и там тоже без
        /// паники.
        #[test]
        fn never_panics_on_consistent_writes(
            fc in proptest::sample::select(vec![0x0F_u8, 0x10]),
            fields in proptest::array::uniform3(proptest::num::u8::ANY),
            data in proptest::collection::vec(proptest::num::u8::ANY, 0..40),
        ) {
            let mut data = data;
            let qty = if fc == 0x0F {
                (data.len() * 8).saturating_sub(usize::from(fields[2] % 8))
            } else {
                data.truncate(data.len() / 2 * 2);
                data.len() / 2
            };
            let qty = u16::try_from(qty).unwrap().to_be_bytes();
            let count = u8::try_from(data.len()).unwrap();
            let pdu = [&[fc, fields[0], fields[1]][..], &qty, &[count], &data].concat();
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let out = rt.block_on(async {
                let mut map = Map::default();
                run(&[(1, &pdu)], &mut map).await
            });
            proptest::prop_assert_eq!(out.len(), 1);
        }
    }
}
