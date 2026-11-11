//! Пароль консоли платы ({% if has_cli == "true" %}узел `CONSOLE`, `cli`{% else %}узел `SSH`{% endif %}): вход всегда по паролю.

/// Хэш пароля консоли — соль и SHA-256(соль ‖ пароль): константа из
/// `app/src/console_password.rs`, которую пишет `post-script.rhai` при
/// генерации проекта. Пароль — имя проекта плюс `00`; самого пароля в
/// прошивке нет.
pub const PASSWORD: domain::console::PasswordHash =
    domain::console::PasswordHash(&crate::console_password::PASSWORD_HASH);

/// Вся заготовка нулевая — `post-script.rhai` её не перезаписал.
const fn placeholder(hash: &[u8; 48]) -> bool {
    let mut i = 0;
    while i < hash.len() {
        if hash[i] != 0 {
            return false;
        }
        i += 1;
    }
    true
}

// Хэш не той длины консоль не пустит никого — лучше узнать это при сборке,
// чем по отказу на плате.
const _: () = assert!(
    PASSWORD.0.len() == domain::console::PASSWORD_HASH_LEN,
    "console_password.rs битый: post-script.rhai должен положить 48 байт"
);
const _: () = assert!(
    !placeholder(&crate::console_password::PASSWORD_HASH),
    "console_password.rs не заполнен: сгенерируйте проект заново или впишите соль и хэш"
);
