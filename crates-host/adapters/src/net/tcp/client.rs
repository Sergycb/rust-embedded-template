//! TCP-клиент — исходящее соединение для служб, которые сами подключаются к
//! серверу.

use core::net::SocketAddrV4;

use embassy_net::Stack;
use embassy_net::tcp::{self, TcpSocket};
use embassy_time::{Duration, Instant, Timer};
use embedded_io_async::{ErrorType, Read, Write};
use ports::dial::Dial;

use super::{IDLE_TIMEOUT, KEEP_ALIVE, TcpError, is_open, reset};

/// Сколько должно прожить соединение, чтобы отказы до него забылись.
const STABLE: Duration = Duration::from_secs(30);

/// Потолок паузы между попытками.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// TCP-клиент — исходящее соединение для служб, которые сами подключаются к
/// серверу (клиент MQTT).
///
/// Отказ сервера ([`Dial`]) он пережидает сам: `dial` — одна попытка, и
/// перед ней — пауза по числу отказов подряд, 0, 1, 2, 4 … до 30 с. Отказ —
/// неудачная попытка и соединение, прожившее меньше 30 с до следующего `dial`
/// (сервер оборвал его сразу — неверный пароль, — и бить в него без паузы
/// нельзя); соединение, прожившее дольше, счёт сбрасывает. Чтение и запись без
/// соединения — ошибка: поток принадлежит соединению, которое открыл `dial`.
/// Пропавший сервер снимают keep-alive и таймаут, как у `TcpServer`.
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

/// Пауза перед попыткой после `failures` отказов подряд: 0, 1, 2, 4 … с, не
/// больше [`MAX_BACKOFF`].
pub(crate) fn backoff(failures: u32) -> Duration {
    match failures {
        0 => Duration::from_secs(0),
        // Сдвиг ограничен: дальше 2⁵ с пауза всё равно упирается в потолок.
        n => Duration::from_secs(1 << (n - 1).min(5)).min(MAX_BACKOFF),
    }
}

#[cfg(test)]
mod tests {
    use core::cell::Cell;

    use embassy_net::tcp::TcpSocket;
    use embassy_time::MockDriver;
    use embedded_io_async::{Read, Write};

    use super::*;
    use crate::net::fakes::{HOST, PORT, leak, pair, with_network};

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

    /// Сервер молчит (адрес в сети, но на SYN не отвечает никто) — `dial` не
    /// висит вечно: через `IDLE_TIMEOUT` это отказ, как и RST.
    #[tokio::test]
    async fn dial_to_a_silent_address_times_out() {
        const SILENT: SocketAddrV4 = SocketAddrV4::new(core::net::Ipv4Addr::new(10, 0, 0, 3), PORT);
        let ((device, device_runner), (_host, host_runner)) = pair();
        let mut client = TcpClient::new(device, leak([0; 64]), leak([0; 64]));
        let failed = Cell::new(false);
        with_network(device_runner, host_runner, async {
            let device_side = async {
                assert!(client.dial(SILENT).await.is_err(), "молчание — отказ");
                failed.set(true);
            };
            let wait_out = async {
                settle().await;
                assert!(!failed.get(), "отказ раньше срока");
                MockDriver::get().advance(IDLE_TIMEOUT + Duration::from_secs(1));
            };
            tokio::join!(device_side, wait_out);
        })
        .await;
        assert!(failed.get());
        assert_eq!(client.failures, 1);
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
}
