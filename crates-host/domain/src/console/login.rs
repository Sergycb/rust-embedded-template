//! Вход в консоль: сверка пароля с хэшем из прошивки. Вход — всегда по паролю,
//! и у SSH, и у `cli`.

use defmt_or_log::warn;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Длина хэша пароля [`PasswordHash`]: 16 байт соли и 32 байта
/// SHA-256(соль ‖ пароль). Тот же формат пишет `cargo xtask` в
/// `console-password.hash` (`secrets.rs`).
pub const PASSWORD_HASH_LEN: usize = SALT_LEN + DIGEST_LEN;

/// Пользователь консоли SSH. У `cli` имени нет — только пароль.
pub const USER: &str = "admin";

/// Длина соли в начале хэша пароля.
const SALT_LEN: usize = 16;

/// Длина SHA-256.
const DIGEST_LEN: usize = 32;

/// Хэш пароля консоли из прошивки: [`PASSWORD_HASH_LEN`] байт, соль и
/// SHA-256(соль ‖ пароль). Самого пароля в прошивке нет; хэш другой длины не
/// пускает никого.
///
/// SHA-256 без KDF (bcrypt, argon2) — потому что пароль генерирует
/// `cargo xtask`: 128 бит из ОС, перебор их по хэшу невозможен и без
/// замедления. Пароль, придуманный человеком, такой защиты не даёт.
#[derive(Debug, Clone, Copy)]
pub struct PasswordHash(pub &'static [u8]);

/// Кого пускать: проверенная форма [`PasswordHash`].
#[derive(Clone, Copy)]
pub(crate) enum Access {
    Password {
        salt: &'static [u8; SALT_LEN],
        digest: &'static [u8; DIGEST_LEN],
    },
    /// Хэш пароля не той длины — никого: пускать без проверки нельзя.
    Nobody,
}

impl Access {
    pub(crate) fn new(hash: PasswordHash) -> Self {
        // Битый хэш — не «пускать всех»: прошивку с хэшем не той длины не
        // соберёт `const`-assert в `app`, здесь — защита для любого другого
        // источника хэша.
        if let Some((salt, digest)) = hash.0.split_first_chunk()
            && let Ok(digest) = digest.try_into()
        {
            Self::Password { salt, digest }
        } else {
            warn!(
                "console: хэш пароля в прошивке не {} байт — вход закрыт",
                PASSWORD_HASH_LEN
            );
            Self::Nobody
        }
    }

    /// Пускать ли с паролем `password`: его SHA-256 с солью сравнивается с
    /// хэшем прошивки за постоянное время.
    pub(crate) fn admits(self, password: &[u8]) -> bool {
        match self {
            // Пустой не пускает, даже будь хэш от пустого пароля: как пустой
            // токен REST (`http::auth::authorized`).
            Self::Password { salt, digest } => {
                !password.is_empty()
                    && Sha256::new_with_prefix(salt)
                        .chain_update(password)
                        .finalize()
                        .as_slice()
                        .ct_eq(digest)
                        .into()
            }
            Self::Nobody => false,
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! Тесты сверки пароля.

    use sha2::{Digest, Sha256};

    use super::{Access, PASSWORD_HASH_LEN, PasswordHash};

    /// Хэш пароля, как его пишет `cargo xtask` в `console-password.hash`:
    /// соль и SHA-256(соль ‖ пароль).
    pub(crate) fn hashed(password: &str) -> &'static [u8] {
        let salt = [7; 16];
        let digest = Sha256::new_with_prefix(salt)
            .chain_update(password)
            .finalize();
        [&salt[..], &digest[..]].concat().leak()
    }

    /// Пускает только верный пароль: регистр, хвостовой пробел, обрезанный и
    /// пустой — нет.
    #[test]
    fn a_password_lets_in_only_itself() {
        let access = Access::new(PasswordHash(hashed("s3cret")));
        for (password, admitted) in [
            ("s3cret", true),
            ("s3cre", false),
            ("s3cret ", false),
            ("S3CRET", false),
            ("", false),
        ] {
            assert_eq!(access.admits(password.as_bytes()), admitted, "{password:?}");
        }
        // Соль — часть хэша: тот же пароль с другой солью не совпадает.
        let mut salted = hashed("s3cret").to_vec();
        salted[0] ^= 1;
        assert!(!Access::new(PasswordHash(salted.leak())).admits(b"s3cret"));
    }

    /// Хэш не той длины не пускает никого; хэш пустого пароля — тоже.
    #[test]
    fn a_broken_hash_or_an_empty_password_lets_no_one_in() {
        for hash in [
            &[][..],
            &[0; PASSWORD_HASH_LEN - 1],
            &[0; PASSWORD_HASH_LEN + 1],
        ] {
            let broken = Access::new(PasswordHash(hash));
            for password in ["", "s3cret"] {
                assert!(!broken.admits(password.as_bytes()), "{} байт", hash.len());
            }
        }
        assert!(!Access::new(PasswordHash(hashed(""))).admits(b""));
    }
}
