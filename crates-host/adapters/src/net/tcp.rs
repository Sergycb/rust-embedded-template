//! TCP-транспорт: [`TcpServer`] и его ошибка.
// {%- if net_services contains "mqtt" %}
//! Клиент — [`TcpClient`].
// {%- endif %}

// {%- if net_services contains "mqtt" %}
use core::net::SocketAddrV4;

// {%- endif %}
use embassy_net::Stack;
use embassy_net::tcp::{self, AcceptError, State, TcpSocket};
use embassy_time::Duration;
// {%- if net_services contains "mqtt" %}
use embassy_time::{Instant, Timer};
// {%- endif %}
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
// {%- if net_services contains "mqtt" %}
use ports::dial::Dial;
// {%- endif %}

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
        is_open(&self.socket)
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
        reset(&mut self.socket).await;
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

/// Почему [`TcpServer`] или `TcpClient` не прочитал или не записал.
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

// {%- if net_services contains "mqtt" %}
/// TCP-клиент — исходящее соединение для служб, которые сами подключаются к
/// серверу (клиент MQTT).
///
/// Отказ сервера ([`Dial`]) он пережидает сам: `dial` — одна попытка, и
/// перед ней — пауза по числу отказов подряд, 0, 1, 2, 4 … до 30 с. Отказ —
/// неудачная попытка и соединение, прожившее меньше 30 с до следующего `dial`
/// (сервер оборвал его сразу — неверный пароль, — и бить в него без паузы
/// нельзя); соединение, прожившее дольше, счёт сбрасывает. Чтение и запись без
/// соединения — ошибка: поток принадлежит соединению, которое открыл `dial`.
/// Пропавший сервер снимают keep-alive и таймаут, как у [`TcpServer`].
pub struct TcpClient {
    socket: TcpSocket<'static>,
    /// Отказов подряд — от него пауза перед следующей попыткой.
    failures: u32,
    /// Когда открыто нынешнее соединение (`None` — его нет).
    connected_at: Option<Instant>,
}

// Вручную: у сокетов `embassy-net` нет `Debug`.
impl core::fmt::Debug for TcpClient {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("TcpClient")
            .field("failures", &self.failures)
            .finish_non_exhaustive()
    }
}

/// Сколько должно прожить соединение, чтобы отказы до него забылись.
const STABLE: Duration = Duration::from_secs(30);

/// Потолок паузы между попытками.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Пауза перед попыткой после `failures` отказов подряд: 0, 1, 2, 4 … с, не
/// больше [`MAX_BACKOFF`].
pub(crate) fn backoff(failures: u32) -> Duration {
    match failures {
        0 => Duration::from_secs(0),
        // Сдвиг ограничен: дальше 2⁵ с пауза всё равно упирается в потолок.
        n => Duration::from_secs(1 << (n - 1).min(5)).min(MAX_BACKOFF),
    }
}

impl TcpClient {
    /// Клиент на стеке `stack` с буферами сокета `rx` и `tx`.
    #[must_use]
    pub fn new(stack: Stack<'static>, rx: &'static mut [u8], tx: &'static mut [u8]) -> Self {
        Self {
            socket: TcpSocket::new(stack, rx, tx),
            failures: 0,
            connected_at: None,
        }
    }
}

impl Dial for TcpClient {
    type Error = tcp::ConnectError;

    async fn dial(&mut self, to: SocketAddrV4) -> Result<(), tcp::ConnectError> {
        if let Some(since) = self.connected_at.take() {
            self.failures = if since.elapsed() >= STABLE {
                0
            } else {
                self.failures.saturating_add(1)
            };
        }
        Timer::after(backoff(self.failures)).await;
        // Прежнее соединение или брошенная попытка: сокет — в `Closed`.
        reset(&mut self.socket).await;
        self.socket.set_keep_alive(Some(KEEP_ALIVE));
        self.socket.set_timeout(Some(IDLE_TIMEOUT));
        let connected = self.socket.connect(to).await;
        match connected {
            Ok(()) => self.connected_at = Some(Instant::now()),
            Err(_) => self.failures = self.failures.saturating_add(1),
        }
        connected
    }

    fn hang_up(&mut self) {
        // RST уйдёт с ближайшим опросом стека; `dial` его дождётся.
        self.socket.abort();
    }
}

impl ErrorType for TcpClient {
    type Error = TcpError;
}

impl Read for TcpClient {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, TcpError> {
        if !is_open(&self.socket) {
            return Err(TcpError::NotConnected);
        }
        self.socket.read(buf).await.map_err(TcpError::Io)
    }
}

impl Write for TcpClient {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, TcpError> {
        if !is_open(&self.socket) {
            return Err(TcpError::NotConnected);
        }
        self.socket.write(buf).await.map_err(TcpError::Io)
    }

    async fn flush(&mut self) -> Result<(), TcpError> {
        if !is_open(&self.socket) {
            return Err(TcpError::NotConnected);
        }
        self.socket.flush().await.map_err(TcpError::Io)
    }
}
// {%- endif %}

#[cfg(test)]
mod tests {
    // {%- if net_services contains "mqtt" %}
    use core::cell::Cell;

    use embassy_time::MockDriver;
    // {%- endif %}
    use embedded_io_async::{ErrorKind, Read, Write};

    use super::*;
    // {%- if net_services contains "mqtt" %}
    use crate::net::fakes::HOST;
    // {%- endif %}
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

    /// Обрыв соединения клиентом: чтение отвечает ошибкой, сокет сброшен в
    /// `Closed`, и запись после этого — `NotConnected`, а не ответ чужому
    /// клиенту.
    #[tokio::test]
    async fn tcp_server_drops_a_reset_connection() {
        let ((device, device_runner), (host, host_runner)) = pair();
        let mut server = TcpServer::new(device, PORT, leak([0; 64]), leak([0; 64]));
        with_network(device_runner, host_runner, async {
            let device_side = async {
                let mut buf = [0; 8];
                assert!(matches!(
                    server.read(&mut buf).await,
                    Err(TcpError::Io(tcp::Error::ConnectionReset))
                ));
                assert_eq!(server.socket.state(), State::Closed);
                assert!(!server.is_connected());
                assert!(matches!(
                    server.write(b"late").await,
                    Err(TcpError::NotConnected)
                ));
            };
            let host_side = async {
                let mut client = TcpSocket::new(host, leak([0; 64]), leak([0; 64]));
                client.connect((DEVICE, PORT)).await.unwrap();
                client.abort();
                client.flush().await.ok();
            };
            tokio::join!(device_side, host_side);
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
    // {%- if net_services contains "mqtt" %}

    /// Сервер на «хосте» пары, к которому подключается [`TcpClient`].
    const SERVER: SocketAddrV4 = SocketAddrV4::new(HOST, PORT);

    #[test]
    fn backoff_grows_to_cap() {
        let secs: Vec<u64> = (0..9).map(|n| backoff(n).as_secs()).collect();
        assert_eq!(secs, [0, 1, 2, 4, 8, 16, 30, 30, 30]);
    }

    /// Клиент подключается к слушающему серверу, и байты доходят.
    ///
    /// Без `flush`: время `mock-driver` стоит, и отложенное подтверждение
    /// сервера не уйдёт — сами байты уходят и так.
    #[tokio::test]
    async fn dial_connects_and_streams() {
        let ((device, device_runner), (host, host_runner)) = pair();
        let mut client = TcpClient::new(device, leak([0; 64]), leak([0; 64]));
        with_network(device_runner, host_runner, async {
            let device_side = async {
                client.dial(SERVER).await.unwrap();
                client.write_all(b"ping").await.unwrap();
            };
            let host_side = async {
                let mut server = TcpSocket::new(host, leak([0; 64]), leak([0; 64]));
                server.accept(PORT).await.unwrap();
                let mut buf = [0; 4];
                server.read_exact(&mut buf).await.unwrap();
                assert_eq!(&buf, b"ping");
            };
            tokio::join!(device_side, host_side);
        })
        .await;
        assert_eq!(client.failures, 0);
    }

    /// Даёт стекам и клиенту поработать, не двигая время.
    async fn settle() {
        for _ in 0..100 {
            tokio::task::yield_now().await;
        }
    }

    /// Сервера нет — `dial` возвращает ошибку (RST), и следующий выдерживает
    /// паузу `backoff(1)`; сервер поднялся — он подключается, и байты доходят.
    #[tokio::test]
    async fn dial_fails_without_server_and_paces_the_next() {
        let ((device, device_runner), (host, host_runner)) = pair();
        let mut client = TcpClient::new(device, leak([0; 64]), leak([0; 64]));
        let dialed = Cell::new(false);
        with_network(device_runner, host_runner, async {
            assert!(client.dial(SERVER).await.is_err(), "сервера нет");
            assert_eq!(client.failures, 1);
            let device_side = async {
                client.dial(SERVER).await.unwrap();
                dialed.set(true);
                client.write_all(b"ping").await.unwrap();
            };
            let host_side = async {
                let mut server = TcpSocket::new(host, leak([0; 64]), leak([0; 64]));
                let wait_out = async {
                    settle().await;
                    assert!(!dialed.get(), "пауза после отказа не выдержана");
                    MockDriver::get().advance(backoff(1));
                };
                let (accepted, ()) = tokio::join!(server.accept(PORT), wait_out);
                accepted.unwrap();
                let mut buf = [0; 4];
                server.read_exact(&mut buf).await.unwrap();
                assert_eq!(&buf, b"ping");
            };
            tokio::join!(device_side, host_side);
        })
        .await;
        assert!(dialed.get());
        assert_eq!(client.failures, 1, "один отказ до подключения");
    }

    /// Соединение, положенное сразу после подключения, — отказ: новый `dial`
    /// выдерживает паузу `backoff(1)`, хотя сервер на месте.
    #[tokio::test]
    async fn quick_redial_is_paced() {
        let ((device, device_runner), (host, host_runner)) = pair();
        let mut client = TcpClient::new(device, leak([0; 64]), leak([0; 64]));
        let redialed = Cell::new(false);
        with_network(device_runner, host_runner, async {
            let mut server = TcpSocket::new(host, leak([0; 64]), leak([0; 64]));
            let (accepted, dialed) = tokio::join!(server.accept(PORT), client.dial(SERVER));
            accepted.unwrap();
            dialed.unwrap();
            client.hang_up();
            // Сервер видит сброс и снова слушает.
            let mut buf = [0; 1];
            assert!(server.read(&mut buf).await.is_err());
            server.abort();
            server.flush().await.ok();

            let device_side = async {
                client.dial(SERVER).await.unwrap();
                redialed.set(true);
            };
            let host_side = async {
                let wait_out = async {
                    settle().await;
                    assert!(!redialed.get(), "быстрый повтор без паузы");
                    MockDriver::get().advance(backoff(1));
                };
                let (accepted, ()) = tokio::join!(server.accept(PORT), wait_out);
                accepted.unwrap();
            };
            tokio::join!(device_side, host_side);
        })
        .await;
        assert_eq!(client.failures, 1);
    }

    /// Чтение и запись без соединения — ошибка, а не ожидание.
    #[tokio::test]
    async fn tcp_client_without_connection_refuses_io() {
        let ((device, _), _) = pair();
        let mut client = TcpClient::new(device, leak([0; 64]), leak([0; 64]));
        assert!(matches!(
            client.read(&mut [0; 4]).await,
            Err(TcpError::NotConnected)
        ));
        assert!(matches!(
            client.write(b"x").await,
            Err(TcpError::NotConnected)
        ));
        assert!(matches!(client.flush().await, Err(TcpError::NotConnected)));
    }
    // {%- endif %}
}
