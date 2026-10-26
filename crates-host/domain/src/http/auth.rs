//! Доступ к REST API: `Authorization: Bearer <токен>` ровно с токеном прошивки.

use picoserve::extract::FromRequestParts;
use picoserve::request::RequestParts;
use picoserve::response::StatusCode;

use super::api::Api;

/// Запрос с верным токеном.
pub(super) struct Auth;

impl<'r> FromRequestParts<'r, Api> for Auth {
    type Rejection = (StatusCode, &'static str);

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "сигнатуру задаёт трейт `picoserve`; проверка токена ничего не ждёт"
    )]
    async fn from_request_parts(
        api: &'r Api,
        parts: &RequestParts<'r>,
    ) -> Result<Self, Self::Rejection> {
        let header = parts.headers().get("authorization").map(|v| v.as_raw());
        if authorized(header, api.token) {
            Ok(Self)
        } else {
            Err((StatusCode::UNAUTHORIZED, "unauthorized\n"))
        }
    }
}

/// `Authorization: Bearer <токен>` ровно с токеном прошивки.
///
/// Схема — без учёта регистра (RFC 7235, §2.1), токен — байт в байт, а не
/// через `HeaderValue == &str`: тот регистра не различает. Сравнение — без
/// выхода на первом несовпавшем байте, чтобы время ответа не подсказывало
/// токен по байту. Пустой токен не пускает никого: иначе `Bearer ` без
/// токена открывал бы API.
pub(super) fn authorized(header: Option<&[u8]>, token: &str) -> bool {
    const SCHEME: &[u8] = b"Bearer ";
    let Some(header) = header else { return false };
    let (scheme, given) = header.split_at(header.len().min(SCHEME.len()));
    let token = token.as_bytes();
    !token.is_empty()
        && scheme.eq_ignore_ascii_case(SCHEME)
        && given.len() == token.len()
        && given
            .iter()
            .zip(token)
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}
