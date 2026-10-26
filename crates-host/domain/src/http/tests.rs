//! Тесты доступа к REST API: проверка токена.

use super::auth::authorized;

#[test]
fn only_the_exact_bearer_token_passes() {
    assert!(authorized(Some(b"Bearer s3cret"), "s3cret"));
    // Схема — без учёта регистра, токен — с ним.
    assert!(authorized(Some(b"bearer s3cret"), "s3cret"));
    for header in [
        &b"Bearer S3CRET"[..],
        b"Bearer s3cret ",
        b"Bearer s3cre",
        b"s3cret",
        b"Bearer ",
        b"Basic s3cret",
    ] {
        assert!(!authorized(Some(header), "s3cret"), "{header:?}");
    }
    assert!(!authorized(None, "s3cret"));
}

/// Пустой токен прошивки — не «пускать всех»: `Bearer ` без токена не
/// проходит.
#[test]
fn an_empty_token_lets_no_one_in() {
    for header in [&b"Bearer "[..], b"Bearer", b""] {
        assert!(!authorized(Some(header), ""), "{header:?}");
    }
}
