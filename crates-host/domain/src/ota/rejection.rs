//! Отказы узла `OTA` для хоста: чужому клиенту — `Busy`, ошибки приёма и
//! применения — в код [`Rejection`].

use defmt_or_log::warn;
use ports::{DownloadError, Rejection, UpdateError};

use crate::ota::Receiving;
use crate::ota::client::{Answer, ClientId};

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
pub(super) fn download_rejection<F>(err: &DownloadError<F>) -> Rejection {
    match err {
        DownloadError::Empty | DownloadError::TooLong { .. } => Rejection::Length,
        DownloadError::TooMuchData { .. }
        | DownloadError::Incomplete { .. }
        | DownloadError::OutOfOrder { .. } => Rejection::Transfer,
        // `UnusableGranularity`, `Flash` и незнакомые варианты (перечисление
        // `non_exhaustive`) сообщаются как отказ устройства, а не прячутся.
        _ => Rejection::Device,
    }
}

/// Код отказа для хоста по ошибке применения (раскладка — в обоих режимах).
pub(super) fn update_rejection<E>(err: &UpdateError<E>) -> Rejection {
    match err {
        UpdateError::TooLong { .. } | UpdateError::Truncated { .. } => Rejection::Length,
        UpdateError::Busy => Rejection::Busy,
        UpdateError::Rollback { .. } => Rejection::Rollback,
        UpdateError::Layout { .. } => Rejection::Layout,
        // `Flash` и незнакомые варианты (перечисление `non_exhaustive`).
        _ => Rejection::Device,
    }
}
