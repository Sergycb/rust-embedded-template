//! Заголовок MBAP: номер транзакции, протокол (0 — Modbus), длина
//! оставшегося (адрес и PDU), адрес устройства — семь байт, старший байт
//! первым.

use ports::MAX_PDU;

/// Длина заголовка MBAP вместе с адресом устройства.
pub(super) const HEADER: usize = 7;

/// Наибольший кадр: заголовок и PDU.
pub(super) const MAX_ADU: usize = HEADER + MAX_PDU;

/// Разобранный заголовок.
#[derive(Debug, Clone, Copy)]
pub(super) struct Header {
    pub(super) tid: u16,
    pub(super) protocol: u16,
    /// Длина адреса и PDU.
    length: u16,
    pub(super) unit: u8,
}

impl Header {
    /// Заголовок из первых [`HEADER`] байт `bytes`.
    pub(super) fn parse(bytes: &[u8]) -> Self {
        let word = |at: usize| u16::from_be_bytes([bytes[at], bytes[at + 1]]);
        Self {
            tid: word(0),
            protocol: word(2),
            length: word(4),
            unit: bytes[6],
        }
    }

    /// Длина кадра целиком; `None` — длина в заголовке вне допустимой:
    /// PDU пустой или длиннее [`MAX_PDU`].
    pub(super) fn total(self) -> Option<usize> {
        let pdu = usize::from(self.length).checked_sub(1)?;
        (1..=MAX_PDU).contains(&pdu).then_some(HEADER + pdu)
    }
}

/// Заголовок кадра с PDU длины `pdu`.
///
/// # Panics
///
/// Если `pdu` длиннее [`MAX_PDU`].
pub(super) fn head(tid: u16, unit: u8, pdu: usize) -> [u8; HEADER] {
    assert!(pdu <= MAX_PDU, "PDU {pdu} > {MAX_PDU} байт");
    #[expect(clippy::cast_possible_truncation, reason = "pdu ≤ 253")]
    let [hi, lo] = (pdu as u16 + 1).to_be_bytes();
    let [t0, t1] = tid.to_be_bytes();
    [t0, t1, 0, 0, hi, lo, unit]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn head_then_parse() {
        let h = Header::parse(&head(0x1234, 0x11, 5));
        assert_eq!(
            (h.tid, h.protocol, h.unit, h.total()),
            (0x1234, 0, 0x11, Some(12))
        );
    }

    #[test]
    fn length_out_of_range() {
        for length in [0_u16, 1, 255, 0xFFFF] {
            let [hi, lo] = length.to_be_bytes();
            let h = Header::parse(&[0, 1, 0, 0, hi, lo, 1]);
            assert_eq!(h.total(), None, "длина {length}");
        }
    }
}
