//! Вход пользователя: кого пускать и разбор его открытого ключа из прошивки.

use defmt_or_log::warn;

/// Пользователь консоли.
pub const USER: &str = "admin";

/// Как плата пускает пользователя [`USER`].
#[derive(Debug, Clone, Copy)]
pub enum Login {
    /// По паролю.
    Password(&'static str),
    /// По открытому ключу: строка `ssh-ed25519 AAAA… [комментарий]` из
    /// `~/.ssh/id_ed25519.pub`.
    Key(&'static str),
}

/// Кого пускать: проверенная форма [`Login`].
#[derive(Clone, Copy)]
pub(super) enum Access {
    Password(&'static str),
    Key([u8; 32]),
    /// Ключ из прошивки не разобрался — никого: пускать без проверки нельзя.
    Nobody,
}

impl Access {
    pub(super) fn new(login: Login) -> Self {
        match login {
            Login::Password(password) => Self::Password(password),
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
