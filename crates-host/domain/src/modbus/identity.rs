//! Идентификация устройства — FC43/14 (Read Device Identification): кто
//! производитель, что за модель и какая прошивка, без карты регистров.

use ports::Exception;

/// Производитель — объект `VendorName` (0x00). Проект ставит своё.
pub const VENDOR: &str = "Vendor";

/// Модель — объект `ProductCode` (0x01). Проект ставит своё.
pub const PRODUCT: &str = "Template device";

/// Функция Encapsulated Interface Transport и её вид Read Device ID.
pub(super) const FUNCTION: u8 = 0x2B;
const MEI_READ_DEVICE_ID: u8 = 0x0E;

/// Чтение базовой категории потоком и одного объекта (коды 1 и 4).
const CONFORMITY: u8 = 0x81;

/// Ответ на запрос FC43 `pdu` в `out`: три объекта базовой категории —
/// [`VENDOR`], [`PRODUCT`] и `revision`, — потоком (код 1) или по одному
/// (код 4); длина PDU ответа.
///
/// Потоку, который не влез в `out`, ставится «продолжение следует» и номер
/// первого непоказанного объекта — с него хост спросит следующим запросом.
/// Номер вне базовой категории в потоке — начало с нулевого, как требует
/// спецификация; при чтении одного объекта — исключение.
///
/// # Errors
///
/// - `IllegalFunction` — другой вид MEI;
/// - `IllegalDataValue` — запрос не той длины или категория не базовая;
/// - `IllegalDataAddress` — нет объекта с таким номером (код 4).
pub(super) fn read_device_id(
    pdu: &[u8],
    revision: &str,
    out: &mut [u8],
) -> Result<usize, Exception> {
    let &[FUNCTION, mei, code, first] = pdu else {
        return Err(Exception::IllegalDataValue);
    };
    if mei != MEI_READ_DEVICE_ID {
        return Err(Exception::IllegalFunction);
    }
    let objects = [VENDOR, PRODUCT, revision];
    let (first, last) = match code {
        1 => (
            if usize::from(first) < objects.len() {
                first
            } else {
                0
            },
            objects.len() - 1,
        ),
        4 if usize::from(first) < objects.len() => (first, usize::from(first)),
        4 => return Err(Exception::IllegalDataAddress),
        _ => return Err(Exception::IllegalDataValue),
    };
    out[..7].copy_from_slice(&[FUNCTION, mei, code, CONFORMITY, 0x00, 0x00, 0]);
    let mut len = 7;
    for (id, object) in objects.iter().enumerate().take(last + 1).skip(first.into()) {
        // Объект длиннее байта длины не бывает: строки — константы и версия.
        let size = object.len().min(usize::from(u8::MAX));
        if len + 2 + size > out.len() {
            out[4] = 0xFF;
            out[5] = u8::try_from(id).unwrap_or(u8::MAX);
            break;
        }
        out[len] = u8::try_from(id).unwrap_or(u8::MAX);
        out[len + 1] = u8::try_from(size).unwrap_or(u8::MAX);
        out[len + 2..len + 2 + size].copy_from_slice(&object.as_bytes()[..size]);
        len += 2 + size;
        out[6] += 1;
    }
    Ok(len)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn object(id: u8, text: &str) -> Vec<u8> {
        let mut bytes = vec![id, u8::try_from(text.len()).unwrap()];
        bytes.extend_from_slice(text.as_bytes());
        bytes
    }

    #[test]
    fn the_basic_stream_carries_all_three_objects() {
        let mut out = [0; 253];
        let len = read_device_id(&[0x2B, 0x0E, 1, 0], "1.2.3", &mut out).unwrap();
        let mut expected = vec![0x2B, 0x0E, 1, 0x81, 0x00, 0x00, 3];
        expected.extend(object(0, VENDOR));
        expected.extend(object(1, PRODUCT));
        expected.extend(object(2, "1.2.3"));
        assert_eq!(out[..len], expected);
    }

    /// Номер вне базовой категории в потоке — с начала; при чтении одного
    /// объекта — исключение.
    #[test]
    fn an_unknown_object_restarts_the_stream_but_not_a_single_read() {
        let mut out = [0; 253];
        let len = read_device_id(&[0x2B, 0x0E, 1, 7], "1.2.3", &mut out).unwrap();
        assert_eq!(out[6], 3, "{:02X?}", &out[..len]);
        assert_eq!(
            read_device_id(&[0x2B, 0x0E, 4, 7], "1.2.3", &mut out),
            Err(Exception::IllegalDataAddress)
        );
    }

    #[test]
    fn a_single_object_is_read_alone() {
        let mut out = [0; 253];
        let len = read_device_id(&[0x2B, 0x0E, 4, 2], "1.2.3", &mut out).unwrap();
        let mut expected = vec![0x2B, 0x0E, 4, 0x81, 0x00, 0x00, 1];
        expected.extend(object(2, "1.2.3"));
        assert_eq!(out[..len], expected);
    }

    /// Не влезло — «продолжение следует» и номер первого непоказанного.
    #[test]
    fn a_stream_that_does_not_fit_says_where_to_continue() {
        let mut out = [0; 7 + 2 + VENDOR.len()];
        let len = read_device_id(&[0x2B, 0x0E, 1, 0], "1.2.3", &mut out).unwrap();
        assert_eq!(len, out.len());
        assert_eq!(out[4..7], [0xFF, 0x01, 1]);
    }

    #[test]
    fn other_categories_and_kinds_are_refused() {
        let mut out = [0; 253];
        for (pdu, refusal) in [
            (&[0x2B, 0x0E, 2, 0][..], Exception::IllegalDataValue),
            (&[0x2B, 0x0E, 3, 0], Exception::IllegalDataValue),
            (&[0x2B, 0x0D, 1, 0], Exception::IllegalFunction),
            (&[0x2B, 0x0E, 1], Exception::IllegalDataValue),
        ] {
            assert_eq!(
                read_device_id(pdu, "1.2.3", &mut out),
                Err(refusal),
                "{pdu:02X?}"
            );
        }
    }
}
