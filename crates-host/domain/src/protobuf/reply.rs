//! Ответы схемы, общие для обработчиков подсистем: `request_id`, отказ и
//! строки в ёмкостях полей.

use proto::{Error, Response, Response_, heapless};

/// Ответ на запрос `request_id`.
pub(super) fn response(request_id: u32, result: Response_::Result) -> Response {
    Response {
        request_id,
        result: Some(result),
    }
}

/// Ответ-отказ с текстом для человека.
pub(crate) fn error(message: &str) -> Response_::Result {
    Response_::Result::Error(Error {
        text: text(message),
    })
}

/// Строка схемы из `s`; не помещается в предел поля (`build.rs` крейта
/// `proto`) — пустая: обрезанная строка выглядела бы как настоящая. Строки
/// [`DeviceInfo`](crate::device::DeviceInfo) длиннее предела отсекает
/// обработчик сведений, так что это касается только текстов ошибок.
pub(super) fn text<const N: usize>(s: &str) -> heapless::String<N> {
    heapless::String::try_from(s).unwrap_or_default()
}
