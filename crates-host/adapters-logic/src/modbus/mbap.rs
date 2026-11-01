//! Заголовок MBAP — [`MbapHeader`] из словаря `rusty-modbus-types`: номер
//! транзакции, протокол (0 — Modbus), длина оставшегося (адрес и PDU), адрес
//! устройства — семь байт, старший байт первым.
//!
//! Здесь — только то, чего у типа нет. Разбор из байтов и в байты:
//! `MbapHeader` читается и пишется трейтами `zerocopy`, а прямой зависимости
//! на него у крейта нет — поля собираются через `From<u16>` и читаются `get`.
//! И проверка длины принятого кадра: `MbapHeader` сверяет длину только при
//! сборке (`try_new`), а `pdu_length`/`adu_length` принятого не проверяют —
//! длина вне допустимой отсюда, [`total`].

use ports::MAX_PDU;
use rusty_modbus_types::{MAX_PDU_SIZE, MAX_TCP_ADU_SIZE, MBAP_HEADER_LEN, MbapHeader};

/// Длина заголовка MBAP вместе с адресом устройства.
pub(super) const HEADER: usize = MBAP_HEADER_LEN;

/// Наибольший кадр: заголовок и PDU.
pub(super) const MAX_ADU: usize = MAX_TCP_ADU_SIZE;

// Предел PDU у словаря и у портов — один и тот же: иначе буфер транспорта
// разошёлся бы с буфером логики.
const _: () = assert!(
    MAX_PDU_SIZE == MAX_PDU,
    "MAX_PDU словаря и портов разошлись"
);

/// Заголовок из первых [`HEADER`] байт `bytes`.
pub(super) fn parse(bytes: &[u8]) -> MbapHeader {
    let word = |at: usize| u16::from_be_bytes([bytes[at], bytes[at + 1]]).into();
    MbapHeader {
        transaction_id: word(0),
        protocol_id: word(2),
        length: word(4),
        unit_id: bytes[6],
    }
}

/// Длина кадра целиком; `None` — длина в заголовке вне допустимой: PDU
/// пустой или длиннее [`MAX_PDU`].
pub(super) fn total(header: MbapHeader) -> Option<usize> {
    let pdu = usize::from(header.pdu_length());
    (header.length.get() != 0 && (1..=MAX_PDU).contains(&pdu)).then(|| header.adu_length())
}
// {%- if modbus_tcp == "true" %}

/// Кадр без PDU — только адрес: длина вне допустимой, но граница кадра
/// известна — он кончается на заголовке.
pub(super) fn is_empty(header: MbapHeader) -> bool {
    header.length.get() == 1
}
// {%- endif %}

/// Заголовок кадра с PDU длины `pdu`.
///
/// # Panics
///
/// Если `pdu` длиннее [`MAX_PDU`].
pub(super) fn head(tid: u16, unit: u8, pdu: usize) -> [u8; HEADER] {
    let header = u16::try_from(pdu)
        .ok()
        .and_then(|len| MbapHeader::try_new(tid, unit, len))
        .unwrap_or_else(|| panic!("PDU {pdu} > {MAX_PDU} байт"));
    let [t0, t1] = header.transaction_id.get().to_be_bytes();
    let [p0, p1] = header.protocol_id.get().to_be_bytes();
    let [l0, l1] = header.length.get().to_be_bytes();
    [t0, t1, p0, p1, l0, l1, header.unit_id]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_then_parse() {
        let h = parse(&head(0x1234, 0x11, 5));
        assert_eq!(
            (
                h.transaction_id.get(),
                h.protocol_id.get(),
                h.unit_id,
                total(h)
            ),
            (0x1234, 0, 0x11, Some(12))
        );
    }

    #[test]
    fn length_out_of_range() {
        for length in [0_u16, 1, 255, 0xFFFF] {
            let [hi, lo] = length.to_be_bytes();
            let h = parse(&[0, 1, 0, 0, hi, lo, 1]);
            assert_eq!(total(h), None, "длина {length}");
        }
    }

    #[test]
    #[should_panic(expected = "PDU 254 > 253")]
    fn too_long_pdu_is_a_bug() {
        let _ignored = head(1, 1, MAX_PDU + 1);
    }
}
