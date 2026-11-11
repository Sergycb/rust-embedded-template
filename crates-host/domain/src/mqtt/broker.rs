//! Адрес брокера: куда подключаться.

/// Куда подключаться.
#[derive(Debug, Clone, Copy)]
pub struct Broker {
    /// IPv4-адрес брокера.
    pub addr: [u8; 4],
    /// Порт брокера.
    pub port: u16,
    /// Имя пользователя; пустое — без входа.
    pub user: &'static str,
    /// Пароль.
    pub password: &'static str,
}
