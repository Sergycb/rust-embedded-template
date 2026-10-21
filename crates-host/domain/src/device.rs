//! Сведения об устройстве — общие у узлов, которые отвечают на «кто ты»:
//! `HOST` (protobuf), `HTTP` (REST).

/// Байт в заводском номере чипа STM32 (96 бит).
pub const UID_BYTES: usize = 12;

/// Сведения об устройстве.
///
/// Приходят снаружи: версию и коммит знает прошивка (`app`), чип — плата
/// (`bsp::ChipSpec`), а не `domain`. Пределы длины строк, если они есть, —
/// у того, кто их отдаёт: схема protobuf проверяет свои в
/// `host::Service::new`.
#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    /// Версия прошивки.
    pub version: &'static str,
    /// Короткий хеш коммита.
    pub commit: &'static str,
    /// Имя чипа.
    pub chip: &'static str,
    /// Заводской номер экземпляра чипа.
    pub uid: [u8; UID_BYTES],
    /// Объём flash чипа, байт.
    pub flash_size: u32,
    /// Частота ядра, Гц; `None` — неизвестна.
    pub sys_hz: Option<u32>,
}
