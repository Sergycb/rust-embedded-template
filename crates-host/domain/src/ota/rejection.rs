//! Отказы узла `OTA` для хоста: чужому клиенту — `Busy`, ошибки приёма и
//! применения — в код [`Rejection`].

use defmt_or_log::warn;

use crate::ota::Receiving;
use crate::ota::client::{Answer, ClientId};
use crate::ota::commands::Rejection;
use crate::ota::download::DownloadError;
use crate::ota::update::UpdateError;

/// Команда приёма не от владельца: `Busy`, приём владельца — и его срок — как
/// были.
pub(super) fn busy(s: Receiving, client: ClientId, served: Answer<'_>) -> Receiving {
    warn!(
        "ota: команда клиента {} отвергнута: идёт приём клиента {}",
        client.0, s.owner.0
    );
    served.respond(Err(Rejection::Busy));
    s
}

/// Код отказа для хоста по ошибке приёма.
pub(super) fn download_rejection(err: &DownloadError) -> Rejection {
    match err {
        DownloadError::Empty | DownloadError::TooLong { .. } => Rejection::Length,
        DownloadError::TooMuchData { .. }
        | DownloadError::Incomplete { .. }
        | DownloadError::OutOfOrder { .. } => Rejection::Transfer,
        // `Flash` и незнакомые варианты (перечисление `non_exhaustive`)
        // сообщаются как отказ устройства, а не прячутся.
        _ => Rejection::Device,
    }
}

/// Код отказа для хоста по ошибке применения.
pub(super) fn update_rejection(err: &UpdateError) -> Rejection {
    match err {
        UpdateError::TooLong { .. } | UpdateError::Truncated { .. } => Rejection::Length,
        UpdateError::Busy => Rejection::Busy,
        UpdateError::Rollback { .. } => Rejection::Rollback,
        UpdateError::Layout { .. } => Rejection::Layout,
        // `Flash` и незнакомые варианты (перечисление `non_exhaustive`).
        _ => Rejection::Device,
    }
}
