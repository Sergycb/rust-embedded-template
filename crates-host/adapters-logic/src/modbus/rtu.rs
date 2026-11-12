//! Протокол Modbus RTU: кадр «адрес, PDU, CRC16», его длина и CRC.
//!
//! Транспорта здесь нет: откуда берутся байты и сколько ждать тишины между
//! ними, решает канал [`Rtu`](crate::modbus::Rtu), который зовёт эти функции.
//!
//! Граница кадра выводится из содержимого: длина — из кода функции и счётчика
//! байт, CRC подтверждает, что кадр найден верно; после битого кадра приём
//! сдвигается на байт, пока CRC не сойдётся снова. Паузу в 3,5 символа между
//! кадрами поток точно не показывает, и канал ловит только долгую тишину
//! посреди начатого кадра ([`FRAME_GAP`]): такой обрывок он выбрасывает, а не
//! ждёт его конца по счётчику байт.

use crc::{CRC_16_MODBUS, Crc, NoTable};
use embassy_time::Duration;
use ports::MAX_PDU;

/// Наибольший кадр RTU: адрес, PDU до 253 байт, CRC.
pub(super) const MAX_ADU: usize = MAX_PDU + 3;

/// Тишина посреди начатого кадра, после которой slave его выбрасывает.
///
/// По спецификации (Modbus over Serial Line 1.02, §2.5.1.1) кадр кончается
/// паузой в 3,5 символа (выше 19200 бод — 1,75 мс), а пауза больше 1,5
/// символа внутри кадра делает его битым. Протокол скорости линии не знает,
/// поэтому константа берётся с запасом на медленную: 20 мс больше 1,5
/// символа на любой скорости от 1200 бод (13,75 мс), так что целый кадр
/// с допустимыми паузами внутри не режется, — и меньше тайм-аута любого
/// master'а (обычно от 100 мс), так что после обрывка slave успевает
/// очиститься до повтора запроса. Та же константа терпит и USB CDC, где
/// кадр приходит пакетами с паузами в миллисекунды. На быстрой линии slave
/// поэтому склеит обрывок со следующим кадром, если тот пришёл раньше
/// 20 мс, — тогда кадр выровняет CRC, как и без паузы.
pub(super) const FRAME_GAP: Duration = Duration::from_millis(20);

/// CRC-16/MODBUS: полином 0xA001 (отражённый 0x8005), начальное значение
/// 0xFFFF. Без таблицы: 512 байт флеша не стоят выигрыша на кадрах до 256
/// байт.
const CRC: Crc<u16, NoTable> = Crc::<u16, NoTable>::new(&CRC_16_MODBUS);

/// Приписывает CRC к первым `len` байтам `buf`; возвращает длину кадра.
pub(super) fn seal(buf: &mut [u8], len: usize) -> usize {
    let crc = CRC.checksum(&buf[..len]).to_le_bytes();
    buf[len..len + 2].copy_from_slice(&crc);
    len + 2
}

// {%- if modbus_master == "true" %}
/// Собирает в `tx` кадр «`unit`, `pdu`, CRC»; возвращает его длину.
pub(super) fn frame(tx: &mut [u8; MAX_ADU], unit: u8, pdu: &[u8]) -> usize {
    assert!(pdu.len() <= MAX_PDU, "PDU {} > {MAX_PDU} байт", pdu.len());
    tx[0] = unit;
    tx[1..=pdu.len()].copy_from_slice(pdu);
    seal(tx, 1 + pdu.len())
}
// {%- endif %}

/// Сколько первых байт запроса нужно, чтобы узнать длину кадра; `None` —
/// функция, длину кадра которой по одному коду функции не вывести.
///
/// Длину знать нужно и для функций, которых slave не исполняет: без неё
/// кадр не найти, CRC не проверить и Illegal Function не ответить — master
/// получил бы таймаут. Поэтому здесь все открытые функции спецификации
/// (Modbus Application Protocol 1.1b3, §6); за бортом только
/// пользовательские (65–72, 100–110), MEI, кроме 14, и FC08/00 с данными
/// длиннее двух байт.
pub(super) fn request_header(fc: u8) -> Option<usize> {
    match fc {
        // Длина — по одному коду функции.
        0x01..=0x08 | 0x0B | 0x0C | 0x11 | 0x16 | 0x18 => Some(2),
        // адрес, функция, счётчик байт (FC20/21) или тип MEI (FC43)
        0x14 | 0x15 | 0x2B => Some(3),
        // адрес, функция, начало, количество, счётчик байт
        0x0F | 0x10 => Some(7),
        // адрес, функция, начало и количество чтения, начало и количество
        // записи, счётчик байт записи
        0x17 => Some(11),
        _ => None,
    }
}

/// Длина кадра запроса по его заголовку из [`request_header`] байт. Может
/// превышать [`MAX_ADU`] — такой кадр не настоящий.
pub(super) fn request_len(header: &[u8]) -> usize {
    match header[1] {
        // Без данных (Serial Line only): адрес, функция, CRC.
        0x07 | 0x0B | 0x0C | 0x11 => 4,
        // Read FIFO Queue: адрес, функция, указатель FIFO, CRC.
        0x18 => 6,
        // Mask Write Register: адрес, функция, регистр, маски AND и OR, CRC.
        0x16 => 10,
        // File Record: адрес, функция, счётчик байт, данные, CRC.
        0x14 | 0x15 => 5 + usize::from(header[2]),
        0x0F | 0x10 => 9 + usize::from(header[6]),
        // Read/Write Multiple Registers: заголовок, данные записи, CRC.
        0x17 => 13 + usize::from(header[10]),
        // Read Device Identification: адрес, функция, MEI, код чтения,
        // объект, CRC.
        0x2B if header[2] == 0x0E => 7,
        // Другие MEI slave не обслуживает, и длину их кадра не вывести.
        0x2B => usize::MAX,
        // FC01–06, FC08 (подфункция и два байта данных): адрес, функция,
        // четыре байта, CRC. У FC08/00 Return Query Data данных бывает и
        // больше — такой кадр не найдётся, и master получит таймаут.
        _ => 8,
    }
}

// {%- if modbus_master == "true" %}
/// Сколько первых байт ответа нужно, чтобы узнать длину кадра; `None` —
/// функция, на которую master не отправляет запросов.
pub(super) fn response_header(fc: u8) -> Option<usize> {
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
pub(super) fn response_len(header: &[u8]) -> usize {
    match header[1] {
        fc if fc & 0x80 != 0 => 5,
        0x01..=0x04 => 5 + usize::from(header[2]),
        _ => 8,
    }
}
// {%- endif %}

/// Сошлась ли CRC в последних двух байтах кадра (младший байт первым).
pub(super) fn crc_ok(frame: &[u8]) -> bool {
    match frame.len().checked_sub(2) {
        Some(body) if body > 0 => {
            CRC.checksum(&frame[..body]).to_le_bytes() == [frame[body], frame[body + 1]]
        }
        _ => false,
    }
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
        assert_eq!(request_header(0x41), None);
        assert_eq!(request_header(0x2B), Some(3));
        assert_eq!(request_len(&[1, 0x2B, 0x0E]), 7);
        assert!(request_len(&[1, 0x2B, 0x0D]) > MAX_ADU);
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
}
