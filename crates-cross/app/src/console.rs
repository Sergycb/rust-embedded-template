//! Пароль консоли платы ({% if has_cli == "true" %}узел `CONSOLE`, `cli`{% else %}узел `SSH`{% endif %}): вход всегда по паролю.

/// Хэш пароля консоли — соль и SHA-256(соль ‖ пароль): файл
/// `console-password.hash` в корне проекта, который создаёт `cargo xtask`
/// (README, «{% if has_cli == "true" %}Консоль `cli`{% else %}Консоль SSH{% endif %}»). Самого пароля в прошивке нет.
pub const PASSWORD: domain::console::PasswordHash =
    domain::console::PasswordHash(include_bytes!("../../../console-password.hash"));

// Хэш не той длины консоль не пустит никого — лучше узнать это при сборке,
// чем по отказу на плате.
const _: () = assert!(
    PASSWORD.0.len() == domain::console::PASSWORD_HASH_LEN,
    "console-password.hash битый: удалите его, и `cargo xtask build` создаст новый \
     из console-password.txt (или новый пароль, если нет и его)"
);
