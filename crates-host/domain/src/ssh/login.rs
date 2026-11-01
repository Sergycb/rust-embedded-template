//! Вход пользователя: кого пускать, сверка пароля с хэшем и разбор открытого
//! ключа из прошивки.

use defmt_or_log::warn;
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

/// Длина хэша пароля [`Login::PasswordHash`]: 16 байт соли и 32 байта
/// SHA-256(соль ‖ пароль). Тот же формат пишет `cargo xtask` в
/// `ssh-password.hash` (`secrets.rs`).
pub const PASSWORD_HASH_LEN: usize = SALT_LEN + DIGEST_LEN;

/// Пользователь консоли.
pub const USER: &str = "admin";

/// Длина соли в начале хэша пароля.
const SALT_LEN: usize = 16;

/// Длина SHA-256.
const DIGEST_LEN: usize = 32;

/// Как плата пускает пользователя [`USER`].
#[derive(Debug, Clone, Copy)]
pub enum Login {
    /// По паролю, которого в прошивке нет — только хэш: [`PASSWORD_HASH_LEN`]
    /// байт, соль и SHA-256(соль ‖ пароль). Хэш другой длины не пускает
    /// никого.
    ///
    /// SHA-256 без KDF (bcrypt, argon2) — потому что пароль генерирует
    /// `cargo xtask`: 128 бит из ОС, перебор их по хэшу невозможен и без
    /// замедления. Пароль, придуманный человеком, такой защиты не даёт.
    PasswordHash(&'static [u8]),
    /// По открытому ключу: строка `ssh-ed25519 AAAA… [комментарий]` из
    /// `~/.ssh/id_ed25519.pub`.
    Key(&'static str),
}

/// Кого пускать: проверенная форма [`Login`].
#[derive(Clone, Copy)]
pub(super) enum Access {
    Password {
        salt: &'static [u8; SALT_LEN],
        digest: &'static [u8; DIGEST_LEN],
    },
    Key([u8; 32]),
    /// Ключ из прошивки не разобрался или хэш пароля не той длины — никого:
    /// пускать без проверки нельзя.
    Nobody,
}

impl Access {
    pub(super) fn new(login: Login) -> Self {
        match login {
            // Битый хэш — не «пускать всех»: прошивку с хэшем не той длины не
            // соберёт `const`-assert в `app`, здесь — защита для любого
            // другого источника `Login`.
            Login::PasswordHash(hash) => {
                if let Some((salt, digest)) = hash.split_first_chunk()
                    && let Ok(digest) = digest.try_into()
                {
                    Self::Password { salt, digest }
                } else {
                    warn!(
                        "ssh: хэш пароля в прошивке не {} байт — вход закрыт",
                        PASSWORD_HASH_LEN
                    );
                    Self::Nobody
                }
            }
            Login::Key(line) => {
                if let Some(key) = ed25519_public_key(line) {
                    Self::Key(key)
                } else {
                    warn!("ssh: ключ пользователя в прошивке не ed25519 — вход закрыт");
                    Self::Nobody
                }
            }
        }
    }

    /// Пускать ли по открытому ключу: `key` — предъявленный ключ ed25519
    /// (`None` — ключ другого типа), `user` сверяет имя клиента с
    /// ожидаемым. Имя спрашивается, только когда ключ свой: ошибка его
    /// разбора — ошибка протокола, а не неудача входа.
    pub(super) fn admits_key<E>(
        self,
        key: Option<[u8; 32]>,
        user: impl FnOnce(&str) -> Result<bool, E>,
    ) -> Result<bool, E> {
        match (self, key) {
            (Self::Key(expected), Some(given)) if given == expected => user(USER),
            _ => Ok(false),
        }
    }

    /// Пускать ли по паролю: `user` сверяет имя клиента с ожидаемым
    /// (`sunset`, `matches_username`), `password` — предъявленный пароль.
    /// Его SHA-256 с солью сравнивается с хэшем прошивки за постоянное время.
    pub(super) fn admits_password(self, user: impl FnOnce(&str) -> bool, password: &[u8]) -> bool {
        match self {
            // Пустой не пускает, даже будь хэш от пустого пароля: как пустой
            // токен REST (`http::auth::authorized`).
            Self::Password { salt, digest } => {
                user(USER)
                    && !password.is_empty()
                    && Sha256::new_with_prefix(salt)
                        .chain_update(password)
                        .finalize()
                        .as_slice()
                        .ct_eq(digest)
                        .into()
            }
            Self::Key(_) | Self::Nobody => false,
        }
    }
}

/// Открытый ключ ed25519 из строки `ssh-ed25519 <base64> [комментарий]`: блоб
/// — имя типа и 32 байта ключа, каждое с длиной впереди. `None` — строка не
/// такая.
#[must_use]
pub fn ed25519_public_key(line: &str) -> Option<[u8; 32]> {
    let mut words = line.split_ascii_whitespace();
    if words.next()? != "ssh-ed25519" {
        return None;
    }
    let mut blob = [0; 51];
    if base64(words.next()?.as_bytes(), &mut blob)? != blob.len() {
        return None;
    }
    let (name, key) = blob.split_at(4 + 11);
    if name != b"\0\0\0\x0bssh-ed25519" || key[..4] != [0, 0, 0, 32] {
        return None;
    }
    key[4..].try_into().ok()
}

/// Декодирует base64 (стандартный алфавит, `=` в конце) в начало `out`;
/// длина результата или `None` — не base64 либо не помещается.
fn base64(text: &[u8], out: &mut [u8]) -> Option<usize> {
    let text = text
        .strip_suffix(b"==")
        .or(text.strip_suffix(b"="))
        .unwrap_or(text);
    let (mut acc, mut bits, mut len) = (0u32, 0, 0);
    for &c in text {
        let v = match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => return None,
        };
        acc = (acc << 6) | u32::from(v);
        bits += 6;
        if bits >= 8 {
            bits -= 8;
            *out.get_mut(len)? = (acc >> bits).to_le_bytes()[0];
            len += 1;
            // Отданные биты — прочь: иначе сдвиг переполнил бы `acc` на
            // шестом символе.
            acc &= (1 << bits) - 1;
        }
    }
    Some(len)
}
