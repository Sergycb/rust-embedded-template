//! Кадр Modbus RTU — адрес, PDU, CRC16 — и его длина по первым байтам.
//!
//! Транспорт здесь — поток (`embedded_io_async::Read`), паузы между кадрами
//! в нём не видно, поэтому граница кадра выводится из содержимого: длина
//! следует из кода функции и счётчика байт, CRC подтверждает, что кадр
//! найден верно.

/// Наибольший кадр RTU: адрес, PDU до 253 байт, CRC.
pub(crate) const MAX_ADU: usize = 256;

/// Сколько первых байт запроса нужно, чтобы узнать длину кадра; `None` —
/// функция, которую slave не поддерживает: длину её кадра по одному коду
/// функции не вывести.
pub(crate) fn request_header(fc: u8) -> Option<usize> {
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
pub(crate) fn request_len(header: &[u8]) -> usize {
    match header[1] {
        0x0F | 0x10 => 9 + usize::from(header[6]),
        _ => 8,
    }
}

/// Сколько первых байт ответа нужно, чтобы узнать длину кадра; `None` —
/// функция, на которую master не отправляет запросов.
pub(crate) fn response_header(fc: u8) -> Option<usize> {
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
pub(crate) fn response_len(header: &[u8]) -> usize {
    match header[1] {
        fc if fc & 0x80 != 0 => 5,
        0x01..=0x04 => 5 + usize::from(header[2]),
        _ => 8,
    }
}

/// CRC-16/MODBUS: полином 0xA001 (отражённый 0x8005), начальное значение
/// 0xFFFF.
pub(crate) fn crc16(bytes: &[u8]) -> u16 {
    let mut crc = 0xFFFF_u16;
    for &byte in bytes {
        crc ^= u16::from(byte);
        for _ in 0..8 {
            crc = if crc & 1 == 0 {
                crc >> 1
            } else {
                (crc >> 1) ^ 0xA001
            };
        }
    }
    crc
}

/// Сошлась ли CRC в последних двух байтах кадра (младший байт первым).
pub(crate) fn crc_ok(frame: &[u8]) -> bool {
    match frame.len().checked_sub(2) {
        Some(body) if body > 0 => {
            crc16(&frame[..body]).to_le_bytes() == [frame[body], frame[body + 1]]
        }
        _ => false,
    }
}

/// Приписывает CRC к первым `len` байтам `buf`; возвращает длину кадра.
pub(crate) fn seal(buf: &mut [u8], len: usize) -> usize {
    let crc = crc16(&buf[..len]).to_le_bytes();
    buf[len..len + 2].copy_from_slice(&crc);
    len + 2
}

#[cfg(test)]
mod tests {
    use super::*;

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

    #[test]
    fn response_lengths() {
        assert_eq!(response_header(0x83), Some(2));
        assert_eq!(response_len(&[1, 0x83]), 5);
        assert_eq!(response_header(0x03), Some(3));
        assert_eq!(response_len(&[1, 0x03, 6]), 11);
        assert_eq!(response_len(&[1, 0x10]), 8);
        assert_eq!(response_header(0x2B), None);
    }
}
