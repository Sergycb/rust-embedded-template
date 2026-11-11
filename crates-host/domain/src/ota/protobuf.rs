//! Обработчик `OtaService`: команды обновления от protobuf-узла.

use core::array::TryFromSliceError;
use core::fmt::Write as _;

use defmt_or_log::warn;
use proto::{Error, OtaChunk, OtaFinish, OtaStart, Reboot, Response_, heapless};

use super::command::{CHUNK_BYTES, Chunk, Rejection, SIGNATURE_BYTES};
use super::{OtaClient, OtaCommand, OtaReply};
use crate::protobuf::error;

// Пределы полей схемы (`proto/build.rs`) — те же, что у команды узлу OTA:
// разойдутся — кусок или подпись, прошедшие разбор, не дойдут до узла.
const _: () = assert!(
    proto::OTA_CHUNK_BYTES == CHUNK_BYTES && proto::OTA_SIGNATURE_BYTES == SIGNATURE_BYTES,
    "пределы OtaChunk.data/OtaStart.signature в proto/build.rs разошлись с ota::command"
);

/// Обработчик команд обновления.
#[derive(Debug)]
pub(crate) struct Handler {
    /// Клиент узла OTA: вход `CMD` и номер транспорта.
    ota: OtaClient,
}

impl Handler {
    /// Обработчик, отдающий команды обновления узлу OTA клиентом `ota`.
    #[must_use]
    pub(crate) const fn new(ota: OtaClient) -> Self {
        Self { ota }
    }

    /// Команда узлу OTA и её исход; узел OTA пропал — отказ устройства.
    async fn ota(&self, command: OtaCommand) -> Response_::Result {
        let reply = self.ota.request(command).await.unwrap_or_else(|_lost| {
            warn!("protobuf: узел OTA не ответил");
            Err(Rejection::Device)
        });
        ota_reply(reply)
    }
}

impl proto::host::HandleOta for Handler {
    async fn ota_start(&mut self, request: &OtaStart) -> Response_::Result {
        match signature(&request.signature) {
            Ok(signature) => {
                self.ota(OtaCommand::Start {
                    len: request.len,
                    signature,
                })
                .await
            }
            Err(_) => error("signature must be 64 bytes"),
        }
    }

    async fn ota_chunk(&mut self, request: &OtaChunk) -> Response_::Result {
        match Chunk::new(request.offset, &request.data) {
            Some(chunk) => self.ota(OtaCommand::Chunk(chunk)).await,
            // Ёмкость поля в схеме та же, что у куска: сюда не доходит.
            None => error("chunk too long"),
        }
    }

    async fn ota_finish(&mut self, _request: &OtaFinish) -> Response_::Result {
        self.ota(OtaCommand::Finish).await
    }

    async fn reboot(&mut self, _request: &Reboot) -> Response_::Result {
        self.ota(OtaCommand::Reboot).await
    }
}

/// Подпись из `ota_start`: пусто — подписи нет (узел OTA отвергнет `Start`),
/// 64 байта — подпись, иначе
/// отказ: обрезанную подпись принять за целую нельзя.
fn signature(bytes: &[u8]) -> Result<Option<[u8; SIGNATURE_BYTES]>, TryFromSliceError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    bytes.try_into().map(Some)
}

/// Исход команды обновления — `ok` или `error` с именем отказа
/// ([`Rejection`]).
fn ota_reply(reply: OtaReply) -> Response_::Result {
    match reply {
        Ok(_) => Response_::Result::Ok(proto::Ok {}),
        Err(rejection) => {
            let mut message = heapless::String::new();
            // Имя варианта короче `ERROR_TEXT_BYTES`; не влезло бы — пустой
            // текст, как у `text`.
            if write!(message, "{rejection:?}").is_err() {
                message.clear();
            }
            Response_::Result::Error(Error { text: message })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ota::command::Progress;

    #[test]
    fn a_signature_is_empty_or_exactly_64_bytes() {
        assert!(signature(&[]).unwrap().is_none());
        assert!(signature(&[0; SIGNATURE_BYTES]).unwrap().is_some());
        assert!(signature(&[0; 8]).is_err());
    }

    /// Исход — `ok` или имя отказа.
    #[test]
    fn an_outcome_becomes_ok_or_the_rejection_name() {
        assert!(matches!(
            ota_reply(Ok(Progress::Started)),
            Response_::Result::Ok(_)
        ));
        let refused = ota_reply(Err(Rejection::NoSession));
        assert!(
            matches!(&refused, Response_::Result::Error(e) if e.text == "NoSession"),
            "{refused:?}"
        );
    }
}
