//! Адрес брокера: куда подключаться и откуда узел его берёт.

/// Куда подключаться.
#[derive(Debug, Clone, Copy)]
pub struct Endpoint {
    /// IPv4-адрес брокера.
    pub addr: [u8; 4],
    /// Порт брокера.
    pub port: u16,
    /// Имя пользователя; пустое — без входа.
    pub user: &'static str,
    /// Пароль.
    pub password: &'static str,
}

/// Откуда узел берёт адрес брокера перед каждым подключением.
#[expect(
    async_fn_in_trait,
    reason = "узел MQTT зовёт адрес на одном исполнителе: `Send` у future не нужен"
)]
pub trait Broker {
    /// Адрес брокера на этот раз.
    async fn endpoint(&mut self) -> Endpoint;
}

/// Адрес, вшитый в прошивку.
#[derive(Debug, Clone, Copy)]
pub struct Fixed(pub Endpoint);

impl Broker for Fixed {
    #[expect(
        clippy::unused_async_trait_impl,
        reason = "сигнатуру задаёт трейт `Broker`; вшитому адресу ждать нечего"
    )]
    async fn endpoint(&mut self) -> Endpoint {
        self.0
    }
}
