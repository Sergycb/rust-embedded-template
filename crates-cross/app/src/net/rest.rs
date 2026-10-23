//! REST API (`domain::http`, пул узлов `HTTP`): токен доступа и число соединений.

/// Соединений REST API разом — копий пула узлов `HTTP`: переменная среды
/// `HTTP_CONNECTIONS` из `[env]` `crates-cross/.cargo/config.toml`. Решает
/// приложение, а не узел: копия узла — одно соединение.
pub const CONNECTIONS: usize = match usize::from_str_radix(env!("HTTP_CONNECTIONS"), 10) {
    Ok(connections) => connections,
    Err(_) => panic!("HTTP_CONNECTIONS в [env] crates-cross/.cargo/config.toml — не число"),
};

/// Токен `Authorization: Bearer` REST API (`domain::http`). Файл в корне
/// проекта создаёт первая `cargo xtask build` (или `lint cross`), и он
/// коммитится: токен всё равно лежит открыто в каждом образе.
pub const TOKEN: &str = include_str!("../../../../rest-token.txt").trim_ascii();

// Пустой файл — не «токена нет», а поломка: `domain::http` такой токен не
// пустит никого, и лучше узнать это при сборке, чем по `401` на плате.
const _: () = assert!(
    !TOKEN.is_empty(),
    "rest-token.txt пуст: удалите файл, и `cargo xtask build` создаст новый токен"
);
