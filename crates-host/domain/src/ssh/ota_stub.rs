//! Заглушка клиента OTA для проекта без OTA: у соединения поле `ota` есть
//! всегда, а типа `crate::ota::OtaClient` в такой сборке нет.

/// Клиент OTA, которого в проекте без OTA не бывает: поле не читается.
#[derive(Debug, Clone, Copy)]
pub(super) struct OtaClient;
