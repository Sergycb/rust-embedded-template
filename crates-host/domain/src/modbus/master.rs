//! Modbus master поверх транспорта [`ports::ModbusClient`]: по функции на
//! запрос.
//!
//! Ответ из-за линии — граница доверия: транспорт сверяет то, что вне PDU
//! (адрес, CRC, номер транзакции), а здесь PDU сверяется с запросом
//! (функция, количество, счётчик байт, эхо записи), и всё, что не сошлось,
//! — [`MasterError::Unexpected`](ports::MasterError::Unexpected), а не
//! паника и не мусор в `out`.
//!
//! Таймаута здесь нет: ждать ответ сколько угодно — дело вызывающего
//! (`embassy_time::with_timeout`). Что делать с опоздавшим ответом на
//! брошенный запрос, решает транспорт (`adapters::modbus`).
//!
//! После широковещательной записи (`unit` 0) ответа нет, и функция
//! возвращается сразу; выдержать паузу, пока slave'ы её выполняют
//! (turnaround delay спецификации, обычно 100–200 мс), — тоже дело
//! вызывающего.

use ports::{Bits, Exception, MAX_PDU, MasterError, ModbusClient, Words};
use rusty_modbus_codec::{ResponsePdu, decode_response};
use rusty_modbus_types::{
    MAX_READ_COILS, MAX_READ_DISCRETE_INPUTS, MAX_READ_REGISTERS, MAX_WRITE_COILS,
    MAX_WRITE_REGISTERS,
};

/// Длина PDU любого запроса FC01–06: функция и два 16-битных поля.
const HEAD: usize = 5;

/// Длина заголовка запроса FC15/16: [`HEAD`] и счётчик байт данных.
const MULTIPLE_HEAD: usize = HEAD + 1;

/// Читает coils `start..start + out.len()` slave'а `unit` в `out` (FC01).
///
/// # Errors
///
/// Отказ транспорта, исключение slave'а, ответ не на этот запрос.
///
/// # Panics
///
/// Если `unit` 0 или `out.len()` не 1..=2000.
pub async fn read_coils<C: ModbusClient>(
    link: &mut C,
    unit: u8,
    start: u16,
    out: &mut [bool],
) -> Result<(), MasterError<C::Error>> {
    read_bits(link, unit, 0x01, start, out, MAX_READ_COILS).await
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
/// Если `unit` 0 или `out.len()` не 1..=2000.
pub async fn read_discrete_inputs<C: ModbusClient>(
    link: &mut C,
    unit: u8,
    start: u16,
    out: &mut [bool],
) -> Result<(), MasterError<C::Error>> {
    read_bits(link, unit, 0x02, start, out, MAX_READ_DISCRETE_INPUTS).await
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
/// Если `unit` 0 или `out.len()` не 1..=125.
pub async fn read_holding<C: ModbusClient>(
    link: &mut C,
    unit: u8,
    start: u16,
    out: &mut [u16],
) -> Result<(), MasterError<C::Error>> {
    read_words(link, unit, 0x03, start, out).await
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
/// Если `unit` 0 или `out.len()` не 1..=125.
pub async fn read_input<C: ModbusClient>(
    link: &mut C,
    unit: u8,
    start: u16,
    out: &mut [u16],
) -> Result<(), MasterError<C::Error>> {
    read_words(link, unit, 0x04, start, out).await
}

/// Пишет один coil (FC05). `unit` 0 — широковещательно, без ответа.
///
/// # Errors
///
/// Отказ транспорта, исключение slave'а, эхо не совпало с запросом.
pub async fn write_coil<C: ModbusClient>(
    link: &mut C,
    unit: u8,
    address: u16,
    value: bool,
) -> Result<(), MasterError<C::Error>> {
    let wire: u16 = if value { 0xFF00 } else { 0x0000 };
    write_single(link, unit, 0x05, address, wire).await
}

/// Пишет один holding register (FC06). `unit` 0 — широковещательно, без
/// ответа.
///
/// # Errors
///
/// Отказ транспорта, исключение slave'а, эхо не совпало с запросом.
pub async fn write_register<C: ModbusClient>(
    link: &mut C,
    unit: u8,
    address: u16,
    value: u16,
) -> Result<(), MasterError<C::Error>> {
    write_single(link, unit, 0x06, address, value).await
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
/// Если `values.len()` не 1..=1968.
pub async fn write_coils<C: ModbusClient>(
    link: &mut C,
    unit: u8,
    start: u16,
    values: &[bool],
) -> Result<(), MasterError<C::Error>> {
    let qty = quantity(values.len(), MAX_WRITE_COILS);
    let bytes = usize::from(qty.div_ceil(8));
    let mut tx = [0_u8; MAX_PDU];
    multiple_head(&mut tx, 0x0F, start, qty, bytes);
    for (i, &value) in values.iter().enumerate() {
        if value {
            tx[MULTIPLE_HEAD + i / 8] |= 1 << (i % 8);
        }
    }
    write_multiple(link, unit, start, qty, &tx[..MULTIPLE_HEAD + bytes]).await
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
/// Если `values.len()` не 1..=123.
pub async fn write_registers<C: ModbusClient>(
    link: &mut C,
    unit: u8,
    start: u16,
    values: &[u16],
) -> Result<(), MasterError<C::Error>> {
    let qty = quantity(values.len(), MAX_WRITE_REGISTERS);
    let bytes = values.len() * 2;
    let mut tx = [0_u8; MAX_PDU];
    multiple_head(&mut tx, 0x10, start, qty, bytes);
    for (slot, value) in tx[MULTIPLE_HEAD..MULTIPLE_HEAD + bytes]
        .as_chunks_mut::<2>()
        .0
        .iter_mut()
        .zip(values)
    {
        *slot = value.to_be_bytes();
    }
    write_multiple(link, unit, start, qty, &tx[..MULTIPLE_HEAD + bytes]).await
}

async fn read_bits<C: ModbusClient>(
    link: &mut C,
    unit: u8,
    fc: u8,
    start: u16,
    out: &mut [bool],
    max: u16,
) -> Result<(), MasterError<C::Error>> {
    let qty = quantity(out.len(), max);
    let mut rx = [0_u8; MAX_PDU];
    let len = read_request(link, unit, fc, start, qty, &mut rx).await?;
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

async fn read_words<C: ModbusClient>(
    link: &mut C,
    unit: u8,
    fc: u8,
    start: u16,
    out: &mut [u16],
) -> Result<(), MasterError<C::Error>> {
    let qty = quantity(out.len(), MAX_READ_REGISTERS);
    let mut rx = [0_u8; MAX_PDU];
    let len = read_request(link, unit, fc, start, qty, &mut rx).await?;
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

/// Запрос чтения `qty` элементов: отправляет и принимает PDU ответа в
/// `rx`; возвращает его длину.
async fn read_request<C: ModbusClient>(
    link: &mut C,
    unit: u8,
    fc: u8,
    start: u16,
    qty: u16,
    rx: &mut [u8; MAX_PDU],
) -> Result<usize, MasterError<C::Error>> {
    assert!(
        unit != 0,
        "чтение с адреса 0: широковещательного чтения нет"
    );
    link.exchange(unit, &head(fc, start, qty), rx).await
}

async fn write_single<C: ModbusClient>(
    link: &mut C,
    unit: u8,
    fc: u8,
    address: u16,
    value: u16,
) -> Result<(), MasterError<C::Error>> {
    let tx = head(fc, address, value);
    if unit == 0 {
        return link.broadcast(&tx).await;
    }
    let mut rx = [0_u8; MAX_PDU];
    let len = link.exchange(unit, &tx, &mut rx).await?;
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

/// PDU из функции и двух 16-битных полей (адрес и количество или значение).
fn head(fc: u8, first: u16, second: u16) -> [u8; HEAD] {
    let ([a, b], [c, d]) = (first.to_be_bytes(), second.to_be_bytes());
    [fc, a, b, c, d]
}

/// Заголовок запроса FC15/16 в `tx`; данные идут с [`MULTIPLE_HEAD`].
fn multiple_head(tx: &mut [u8], fc: u8, start: u16, qty: u16, bytes: usize) {
    tx[..HEAD].copy_from_slice(&head(fc, start, qty));
    // Не больше 246 байт: `quantity` уже ограничил количество.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "bytes ≤ 246 после quantity()"
    )]
    let count = bytes as u8;
    tx[HEAD] = count;
}

async fn write_multiple<C: ModbusClient>(
    link: &mut C,
    unit: u8,
    start: u16,
    qty: u16,
    tx: &[u8],
) -> Result<(), MasterError<C::Error>> {
    if unit == 0 {
        return link.broadcast(tx).await;
    }
    let fc = tx[0];
    let mut rx = [0_u8; MAX_PDU];
    let len = link.exchange(unit, tx, &mut rx).await?;
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

/// Разбирает PDU ответа на функцию `fc`; исключение на неё — ошибка.
fn decode<E>(pdu: &[u8], fc: u8) -> Result<ResponsePdu<'_>, MasterError<E>> {
    match decode_response(pdu) {
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
    use crate::modbus::Slave;
    use crate::modbus::fakes::{Client, Map};
    use crate::server::Handler;

    /// Запрос master'а → наш slave → ответ обратно master'у: две реализации
    /// проверяют друг друга, и ни одна не знает, как устроена другая.
    async fn through_slave<F, Fut>(map: &mut Map, call: F) -> Result<(), MasterError<Infallible>>
    where
        F: Fn(Client) -> Fut,
        Fut: Future<Output = (Result<(), MasterError<Infallible>>, Client)>,
    {
        // Первый прогон: ответа нет, master отправляет запрос и упирается в
        // конец потока.
        let (first, client) = call(Client::default()).await;
        assert_eq!(first, Err(MasterError::Eof));
        let (unit, request) = &client.sent[0];
        let mut frame = [0; MAX_PDU + 1];
        frame[0] = *unit;
        frame[1..=request.len()].copy_from_slice(request);
        let mut slave = Slave::new(1, core::mem::take(map), "1.2.3");
        let len = slave
            .handle(&mut frame, 1 + request.len())
            .await
            .expect("slave ответил");
        *map = slave.into_regs();
        call(Client::replying(&frame[1..len])).await.0
    }

    #[tokio::test]
    async fn holding_roundtrip_through_slave() {
        let mut map = Map::default();
        map.holding[5..8].copy_from_slice(&[1, 0xBEEF, 3]);
        let got = std::cell::RefCell::new([0_u16; 3]);
        let result = through_slave(&mut map, |mut link| {
            let got = &got;
            async move {
                let mut out = [0; 3];
                let r = read_holding(&mut link, 1, 5, &mut out).await;
                *got.borrow_mut() = out;
                (r, link)
            }
        })
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(*got.borrow(), [1, 0xBEEF, 3]);
    }

    #[tokio::test]
    async fn write_registers_roundtrip_through_slave() {
        let mut map = Map::default();
        let result = through_slave(&mut map, |mut link| async move {
            (write_registers(&mut link, 1, 2, &[10, 20]).await, link)
        })
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(map.holding[2..4], [10, 20]);
    }

    #[tokio::test]
    async fn coils_roundtrip_through_slave() {
        let mut map = Map::default();
        let values = [true, false, true, true, false, false, false, false, true];
        let result = through_slave(&mut map, |mut link| async move {
            (write_coils(&mut link, 1, 3, &values).await, link)
        })
        .await;
        assert_eq!(result, Ok(()));
        assert_eq!(map.coils[3..12], values);
    }

    #[tokio::test]
    async fn request_pdu_is_spec_example() {
        // Пример из спецификации: прочитать 3 регистра с 0x006B у slave'а 17.
        let mut link = Client::default();
        let _ignored = read_holding(&mut link, 0x11, 0x6B, &mut [0; 3]).await;
        assert_eq!(link.sent, [(0x11, vec![0x03, 0x00, 0x6B, 0x00, 0x03])]);
    }

    #[tokio::test]
    async fn exception_is_reported() {
        for (code, exception) in [
            (0x02, Exception::IllegalDataAddress),
            (0x0B, Exception::GatewayTargetFailed),
        ] {
            let mut link = Client::replying(&[0x83, code]);
            let r = read_holding(&mut link, 1, 0, &mut [0; 1]).await;
            assert_eq!(r, Err(MasterError::Exception(exception)));
        }
    }

    #[tokio::test]
    async fn mismatched_responses_are_unexpected() {
        let cases: [&[u8]; 6] = [
            // просили 3 регистра — пришёл 1
            &[0x03, 2, 0, 7],
            // пустые данные
            &[0x03, 0],
            // ответ на другую функцию
            &[0x04, 6, 0, 1, 0, 2, 0, 3],
            // исключение на другую функцию
            &[0x84, 0x02],
            // функция, которой master не знает
            &[0x2B, 0x0E],
            // пустой PDU
            &[],
        ];
        for reply in cases {
            let mut link = Client::replying(reply);
            let r = read_holding(&mut link, 1, 0, &mut [0; 3]).await;
            assert_eq!(r, Err(MasterError::Unexpected), "ответ {reply:02X?}");
        }
    }

    #[tokio::test]
    async fn broadcast_write_does_not_wait() {
        let mut link = Client::default();
        assert_eq!(write_register(&mut link, 0, 1, 2).await, Ok(()));
        assert_eq!(write_coils(&mut link, 0, 0, &[true]).await, Ok(()));
        assert_eq!(
            link.sent,
            [
                (0, vec![0x06, 0, 1, 0, 2]),
                (0, vec![0x0F, 0, 0, 0, 1, 1, 1])
            ]
        );
    }

    #[tokio::test]
    #[should_panic(expected = "126 элементов")]
    async fn too_many_registers_is_a_bug() {
        let _ignored = read_holding(&mut Client::default(), 1, 0, &mut [0; 126]).await;
    }

    #[tokio::test]
    #[should_panic(expected = "широковещательного чтения нет")]
    async fn broadcast_read_is_a_bug() {
        let _ignored = read_holding(&mut Client::default(), 0, 0, &mut [0; 1]).await;
    }

    proptest::proptest! {
        /// Какой бы PDU ни пришёл в ответ — master не паникует.
        #[test]
        fn never_panics(reply in proptest::collection::vec(proptest::num::u8::ANY, 0..=MAX_PDU)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let _ignored = read_holding(&mut Client::replying(&reply), 1, 0, &mut [0; 4]).await;
                let _ignored = read_coils(&mut Client::replying(&reply), 1, 0, &mut [false; 20]).await;
                let _ignored = write_coils(&mut Client::replying(&reply), 1, 0, &[true; 3]).await;
            });
        }

        /// Ответ согласованной длины со случайным содержимым доходит до
        /// разбора и сверки с запросом — и там тоже без паники.
        #[test]
        fn never_panics_on_well_formed(
            fc in proptest::sample::select(vec![0x01_u8, 0x02, 0x03, 0x04, 0x05, 0x06, 0x0F, 0x10, 0x83]),
            data in proptest::collection::vec(proptest::num::u8::ANY, 0..60),
        ) {
            let reply: Vec<u8> = match fc {
                0x01..=0x04 => {
                    let count = u8::try_from(data.len()).unwrap();
                    [&[fc, count][..], &data].concat()
                }
                0x83 => vec![fc, data.first().copied().unwrap_or(0)],
                _ => [&[fc][..], &data, &[0; 4]].concat()[..5].to_vec(),
            };
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let _ignored = read_holding(&mut Client::replying(&reply), 1, 0, &mut [0; 4]).await;
                let _ignored = read_discrete_inputs(&mut Client::replying(&reply), 1, 0, &mut [false; 20]).await;
                let _ignored = write_register(&mut Client::replying(&reply), 1, 0, 1).await;
                let _ignored = write_coils(&mut Client::replying(&reply), 1, 0, &[true; 3]).await;
            });
        }
    }
}
