//! Пакет `host`: код, сгенерированный из `proto/host.proto`.

// Сгенерированный код линтам проекта не следует и править его нельзя;
// `allow`, а не `expect`: какие именно линты сработают, зависит от схемы и
// версии генератора. `irrefutable_let_patterns` — когда у `oneof` один
// вариант: так у `Request` без OTA (одна команда `info`), и `if let` в
// разборе становится безальтернативным.
#[allow(
    clippy::all,
    clippy::pedantic,
    clippy::arbitrary_source_item_ordering,
    irrefutable_let_patterns,
    nonstandard_style,
    unused,
    missing_debug_implementations,
    missing_docs,
    reason = "код сгенерирован micropb-gen из proto/host.proto"
)]
mod generated {
    include!(concat!(env!("OUT_DIR"), "/host.rs"));
}

pub use generated::host_::*;

#[cfg(test)]
mod tests {
    use micropb::{MessageDecode, MessageEncode, PbEncoder};

    use super::*;

    fn encode(message: &impl MessageEncode) -> Vec<u8> {
        let mut encoder = PbEncoder::new(heapless::Vec::<u8, 128>::new());
        message.encode(&mut encoder).unwrap();
        encoder.into_writer().to_vec()
    }

    /// Эталонные байты по спецификации protobuf, посчитанные вручную, а не
    /// этим же кодеком: `08 07` — поле 1 varint 7, `12 00` — поле 2
    /// length-delimited длины 0.
    #[test]
    fn get_request_matches_the_wire_format() {
        let request = Request {
            request_id: 7,
            command: Some(Request_::Command::Info(Get::default())),
        };
        assert_eq!(encode(&request), [0x08, 0x07, 0x12, 0x00]);

        let mut decoded = Request::default();
        decoded
            .decode_from_bytes(&[0x08, 0x07, 0x12, 0x00])
            .unwrap();
        assert_eq!(decoded, request);
    }

    /// `22 03 0A 01 78` — поле 4 (`error`) длины 3, внутри поле 1 (`text`)
    /// длины 1, «x».
    #[test]
    fn error_response_matches_the_wire_format() {
        let error = Error {
            text: heapless::String::try_from("x").unwrap(),
        };
        let response = Response {
            request_id: 0,
            result: Some(Response_::Result::Error(error)),
        };
        assert_eq!(encode(&response), [0x22, 0x03, 0x0A, 0x01, 0x78]);
    }

    /// Константы ёмкости — те самые, что стоят в сгенерированных типах.
    #[test]
    fn capacity_constants_match_the_generated_types() {
        let info = Info::default();
        assert_eq!(info.version.capacity(), crate::VERSION_BYTES);
        assert_eq!(info.commit.capacity(), crate::COMMIT_BYTES);
        assert_eq!(info.chip.capacity(), crate::CHIP_BYTES);
        assert_eq!(info.uid.capacity(), crate::UID_BYTES);
        assert_eq!(Error::default().text.capacity(), crate::ERROR_TEXT_BYTES);
    }

    /// Строка длиннее ёмкости из `build.rs` не разбирается, а не обрезается.
    #[test]
    fn too_long_string_is_rejected() {
        let mut wire = vec![0x22, 66, 0x0A, 64];
        wire.extend_from_slice(&[b'a'; 64]);
        let mut ok = Response::default();
        ok.decode_from_bytes(&wire).unwrap();

        let mut wire = vec![0x22, 67, 0x0A, 65];
        wire.extend_from_slice(&[b'a'; 65]);
        let mut too_long = Response::default();
        assert!(too_long.decode_from_bytes(&wire).is_err());
    }
}
