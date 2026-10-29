//! TCP-транспорт: общая ошибка и обращение с сокетом.
// {%- if has_tcp == "true" %}
//! Сервер транспортов протоколов — [`TcpServer`] (`server`).
// {%- endif %}
// {%- if net_services contains "mqtt" %}
//! Клиент служб — [`TcpClient`] (`client`).
// {%- endif %}

// {%- if net_services contains "mqtt" %}
mod client;
// {%- endif %}
// {%- if has_tcp == "true" %}
mod server;
// {%- endif %}

use embassy_net::tcp::{self, AcceptError, State, TcpSocket};
use embassy_time::Duration;
use embedded_io_async::ErrorKind;

// {%- if net_services contains "mqtt" %}
pub use client::TcpClient;
// {%- endif %}
// {%- if has_tcp == "true" %}
pub use server::TcpServer;
// {%- endif %}

/// Как часто проверять молчащего собеседника (keep-alive TCP).
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// Через сколько без ответа на keep-alive и данные соединение считается
/// пропавшим: три проверки подряд.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Почему `TcpServer` или `TcpClient` не прочитал или не записал.
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum TcpError {
    /// Клиента не удалось принять.
    Accept(AcceptError),
    /// Соединение оборвалось; следующее чтение примет новое.
    Io(tcp::Error),
    /// Запись без соединения: клиент, которому шёл ответ, уже отключился.
    NotConnected,
}

impl core::fmt::Display for TcpError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Accept(err) => write!(f, "клиент не принят: {err:?}"),
            Self::Io(err) => write!(f, "соединение оборвалось: {err:?}"),
            Self::NotConnected => f.write_str("нет соединения"),
        }
    }
}

impl core::error::Error for TcpError {}

impl embedded_io_async::Error for TcpError {
    fn kind(&self) -> ErrorKind {
        match self {
            // Ошибка программы, а не сети: порт 0 или сокет в чужом состоянии.
            Self::Accept(AcceptError::InvalidPort | AcceptError::InvalidState) => ErrorKind::Other,
            Self::Accept(AcceptError::ConnectionReset) | Self::Io(_) => ErrorKind::ConnectionReset,
            Self::NotConnected => ErrorKind::NotConnected,
        }
    }
}

/// Есть ли у сокета соединение, по которому можно читать и писать.
fn is_open(socket: &TcpSocket<'_>) -> bool {
    matches!(
        socket.state(),
        State::Established | State::CloseWait | State::FinWait1 | State::FinWait2
    )
}

/// Сбрасывает соединение сокета и дожидается ухода RST: сокет снова свободен.
async fn reset(socket: &mut TcpSocket<'_>) {
    socket.abort();
    // Ошибка здесь — то же отсутствие соединения, которое и сбрасываем.
    let _flushed = socket.flush().await;
}
