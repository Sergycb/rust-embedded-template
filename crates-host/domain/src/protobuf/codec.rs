//! Разбор запроса и кодирование ответа: причина отказа разбора строкой и
//! запись сообщения в буфер ответа.

use micropb::{DecodeError, MessageEncode, PbEncoder};

/// Почему кадр не разобрался — строкой: у `micropb::DecodeError` нет
/// `defmt::Format`, а `Debug2Format` втянул бы в прошивку `core::fmt` ради
/// одной строки лога.
pub(super) fn decode_reason<E>(err: &DecodeError<E>) -> &'static str {
    match err {
        DecodeError::VarIntLimit => "varint длиннее 10 байт",
        DecodeError::UnexpectedEof => "кадр оборвался",
        DecodeError::Deprecation => "устаревший тип поля",
        DecodeError::UnknownWireType => "неизвестный тип поля",
        DecodeError::ZeroField => "поле с номером 0",
        DecodeError::CustomField => "поле не разобралось",
        DecodeError::Utf8 => "строка не UTF-8",
        DecodeError::Capacity => "поле не помещается в контейнер",
        DecodeError::WrongLen => "длина поля не сходится",
        DecodeError::Reader(_) => "ошибка чтения",
        // `DecodeError` — `#[non_exhaustive]`: новая причина в `micropb`.
        _ => "кадр не разобрался",
    }
}

/// Кодирует `message` в начало `buf`; `None` — не поместилось.
pub(super) fn encode(message: &impl MessageEncode, buf: &mut [u8]) -> Option<usize> {
    let capacity = buf.len();
    let mut encoder = PbEncoder::new(buf);
    message.encode(&mut encoder).ok()?;
    Some(capacity - encoder.into_writer().len())
}
