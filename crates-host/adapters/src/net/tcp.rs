//! TCP-транспорт: [`TcpServer`] и его ошибка.

use embassy_net::Stack;
use embassy_net::tcp::{self, AcceptError, State, TcpSocket};
use embassy_time::Duration;
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};

/// TCP-сервер на одном порту — поток байтов для транспортов протоколов
/// (`modbus::Tcp`, `cobs::Cobs`).
///
/// Соединение — его забота, а не протокола: чтение ждёт клиента (`accept`),
/// а соединение, закрытое клиентом или оборванное, сбрасывается — протокол
/// видит конец потока, и следующее чтение принимает нового клиента. Запись
/// без соединения — ошибка, а не ожидание клиента: ответ ушедшему клиенту
/// не должен достаться следующему. Клиент один: второй ждёт, пока первый не
/// отключится. Молчаливо пропавшего клиента (обесточен, вынут кабель)
/// снимают keep-alive и таймаут соединения — иначе сокет занят навсегда.
pub struct TcpServer {
    socket: TcpSocket<'static>,
    port: u16,
}

// Вручную: у сокетов `embassy-net` нет `Debug`.
impl core::fmt::Debug for TcpServer {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TcpServer")
            .field("port", &self.port)
            .finish_non_exhaustive()
    }
}

/// Как часто проверять молчащего клиента (keep-alive TCP).
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// Через сколько без ответа на keep-alive и данные соединение считается
/// пропавшим: три проверки подряд.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

impl TcpServer {
    /// Сервер на порту `port` стека `stack` с буферами сокета `rx` и `tx`.
    #[must_use]
    pub fn new(
        stack: Stack<'static>,
        port: u16,
        rx: &'static mut [u8],
        tx: &'static mut [u8],
    ) -> Self {
        Self {
            socket: TcpSocket::new(stack, rx, tx),
            port,
        }
    }

    /// Есть ли соединение, по которому можно читать и писать.
    fn is_connected(&self) -> bool {
        matches!(
            self.socket.state(),
            State::Established | State::CloseWait | State::FinWait1 | State::FinWait2
        )
    }

    /// Ждёт клиента, если соединения нет.
    async fn connected(&mut self) -> Result<(), TcpError> {
        if self.is_connected() {
            return Ok(());
        }
        // Брошенный `accept` оставляет сокет в `Listen`, а повторный из
        // `Listen` — ошибка состояния: сначала — в `Closed`.
        if self.socket.state() != State::Closed {
            self.drop_connection().await;
        }
        self.socket.set_keep_alive(Some(KEEP_ALIVE));
        self.socket.set_timeout(Some(IDLE_TIMEOUT));
        self.socket
            .accept(self.port)
            .await
            .map_err(TcpError::Accept)
    }

    /// Сбрасывает соединение: следующее обращение примет новое.
    async fn drop_connection(&mut self) {
        self.socket.abort();
        // Ошибка здесь — то же отсутствие соединения, которое и сбрасываем.
        let _flushed = self.socket.flush().await;
    }
}

/// Почему [`TcpServer`] не прочитал или не записал.
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

impl ErrorType for TcpServer {
    type Error = TcpError;
}

impl Read for TcpServer {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TcpError> {
        // Пустой буфер — 0 и у живого соединения, концом потока он не считается.
        if buf.is_empty() {
            return Ok(0);
        }
        self.connected().await?;
        match self.socket.read(buf).await {
            Ok(0) => {
                self.drop_connection().await;
                Ok(0)
            }
            Ok(n) => Ok(n),
            Err(err) => {
                self.drop_connection().await;
                Err(TcpError::Io(err))
            }
        }
    }
}

impl Write for TcpServer {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, TcpError> {
        if !self.is_connected() {
            return Err(TcpError::NotConnected);
        }
        match self.socket.write(buf).await {
            Ok(n) => Ok(n),
            Err(err) => {
                self.drop_connection().await;
                Err(TcpError::Io(err))
            }
        }
    }

    async fn flush(&mut self) -> Result<(), TcpError> {
        if !self.is_connected() {
            return Err(TcpError::NotConnected);
        }
        match self.socket.flush().await {
            Ok(()) => Ok(()),
            Err(err) => {
                self.drop_connection().await;
                Err(TcpError::Io(err))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use embedded_io_async::{ErrorKind, Read, Write};

    use super::*;
    use crate::net::fakes::{DEVICE, PORT, leak, pair, with_network};

    /// Сервер принимает клиента, отвечает ему, видит его отключение концом
    /// потока — и принимает следующего на том же сокете.
    ///
    /// Время `mock-driver` стоит, и отложенное подтверждение `smoltcp` не
    /// уходит никогда: `server.flush()` дожидается подтверждения ответа
    /// только потому, что его несёт FIN клиента. Порядок на стороне клиента
    /// (прочитать, закрыть) этим и задан.
    #[tokio::test]
    async fn tcp_server_serves_and_takes_the_next_client() {
        let ((device, device_runner), (host, host_runner)) = pair();
        let mut server = TcpServer::new(device, PORT, leak([0; 256]), leak([0; 256]));
        with_network(device_runner, host_runner, async {
            for round in 0..2_u8 {
                // Сервер слушает с первого чтения — как узел протокола,
                // который читает сразу после старта, — поэтому обе стороны
                // идут одновременно.
                let device_side = async {
                    let mut buf = [0; 8];
                    let n = server.read(&mut buf).await.unwrap();
                    assert_eq!(buf[..n], [round, 1, 2]);
                    server.write_all(b"ok").await.unwrap();
                    server.flush().await.unwrap();
                    assert_eq!(
                        server.read(&mut buf).await.unwrap(),
                        0,
                        "раунд {round}: конец потока"
                    );
                };
                let host_side = async {
                    let mut client = TcpSocket::new(host, leak([0; 256]), leak([0; 256]));
                    client.connect((DEVICE, PORT)).await.unwrap();
                    client.write_all(&[round, 1, 2]).await.unwrap();
                    client.flush().await.unwrap();
                    let mut reply = [0; 2];
                    client.read_exact(&mut reply).await.unwrap();
                    assert_eq!(&reply, b"ok");
                    client.close();
                    client.flush().await.unwrap();
                };
                tokio::join!(device_side, host_side);
            }
        })
        .await;
    }

    /// Запись без соединения — ошибка, а не ожидание клиента; пустой буфер
    /// — 0 без попытки принять соединение.
    #[tokio::test]
    async fn tcp_server_without_client_refuses_writes() {
        let ((device, _), _) = pair();
        let mut server = TcpServer::new(device, PORT, leak([0; 64]), leak([0; 64]));
        assert!(matches!(
            server.write(b"late").await,
            Err(TcpError::NotConnected)
        ));
        assert!(matches!(server.flush().await, Err(TcpError::NotConnected)));
        assert_eq!(server.read(&mut []).await.unwrap(), 0);
        assert_eq!(
            embedded_io_async::Error::kind(&TcpError::NotConnected),
            ErrorKind::NotConnected
        );
        assert_eq!(
            embedded_io_async::Error::kind(&TcpError::Accept(AcceptError::InvalidPort)),
            ErrorKind::Other
        );
        assert_eq!(
            embedded_io_async::Error::kind(&TcpError::Io(tcp::Error::ConnectionReset)),
            ErrorKind::ConnectionReset
        );
    }
}
