//! Сеть поверх `embassy-net`: заглушка сетевого устройства и транспорты
//! протоколов — TCP-сервер и UDP-канал.
//!
//! Ничего из этого не знает чипа: устройство — любой
//! [`Driver`](embassy_net::driver::Driver), сокеты — стек `embassy-net`.
//! Поэтому здесь, а не в `bsp`: `bsp` только собирает стек над своим
//! Ethernet и отдаёт эти объекты в `Board`.

use core::task::Context;

use embassy_net::Stack;
use embassy_net::driver::{Capabilities, Driver, HardwareAddress, LinkState, RxToken, TxToken};
use embassy_net::tcp::{self, AcceptError, State, TcpSocket};
use embassy_net::udp::{PacketMetadata, RecvError, SendError, UdpMetadata, UdpSocket};
use embassy_time::Duration;
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use ports::{FrameLink, LinkError};

/// Сетевое устройство, которого ещё нет на плате, — место под Ethernet.
///
/// Как `serial::Unconnected` в слоте UART, но на уровень ниже: стек
/// `embassy-net`, сокеты и узел `NET` настоящие, а у устройства линк всегда
/// опущен — кадров нет ни на приёме, ни на передаче. Проект с ним
/// собирается и запускается, протокол ждёт соединения вечно. Заменить его —
/// значит в `bsp::net` подставить `Ethernet` своей платы.
#[derive(Debug)]
pub struct Unconnected {
    mac: [u8; 6],
}

impl Unconnected {
    /// Устройство с адресом `mac` — его увидит стек, хотя в сеть ничего не
    /// уходит.
    #[must_use]
    pub const fn new(mac: [u8; 6]) -> Self {
        Self { mac }
    }
}

/// Кадр, которого не бывает: токены приёма и передачи [`Unconnected`].
#[derive(Debug)]
pub enum NoFrame {}

impl RxToken for NoFrame {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, _: F) -> R {
        match self {}
    }
}

impl TxToken for NoFrame {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, _: usize, _: F) -> R {
        match self {}
    }
}

/// MTU Ethernet: 1500 байт IP и 14 байт заголовка.
const ETHERNET_MTU: usize = 1514;

impl Driver for Unconnected {
    type RxToken<'a> = NoFrame;
    type TxToken<'a> = NoFrame;

    fn receive(&mut self, _: &mut Context) -> Option<(NoFrame, NoFrame)> {
        None
    }

    fn transmit(&mut self, _: &mut Context) -> Option<NoFrame> {
        None
    }

    fn link_state(&mut self, _: &mut Context) -> LinkState {
        LinkState::Down
    }

    fn capabilities(&self) -> Capabilities {
        let mut capabilities = Capabilities::default();
        capabilities.max_transmission_unit = ETHERNET_MTU;
        capabilities
    }

    fn hardware_address(&self) -> HardwareAddress {
        HardwareAddress::Ethernet(self.mac)
    }
}

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

/// UDP-канал на одном порту — [`FrameLink`] для протоколов, которым
/// датаграмма и есть кадр (`modbus::Udp`, узел HOST).
///
/// Отвечает тому, кто прислал последнюю датаграмму: адрес отправителя порт
/// не передаёт, а протоколы отвечают на запрос.
pub struct UdpLink {
    socket: UdpSocket<'static>,
    peer: Option<UdpMetadata>,
}

// Вручную: у сокетов `embassy-net` нет `Debug`.
impl core::fmt::Debug for UdpLink {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UdpLink").finish_non_exhaustive()
    }
}

impl UdpLink {
    /// Канал на порту `port` стека `stack`: буферы приёма и передачи и их
    /// метаданные — по датаграмме на запись.
    ///
    /// # Panics
    ///
    /// Если `port` 0 — сокет без порта не принимает ничего.
    #[must_use]
    pub fn new(
        stack: Stack<'static>,
        port: u16,
        rx_meta: &'static mut [PacketMetadata],
        rx: &'static mut [u8],
        tx_meta: &'static mut [PacketMetadata],
        tx: &'static mut [u8],
    ) -> Self {
        assert!(port != 0, "UDP-канал на порту 0");
        let mut socket = UdpSocket::new(stack, rx_meta, rx, tx_meta, tx);
        // Свежий сокет с ненулевым портом привязывается всегда.
        let bound = socket.bind(port);
        assert!(bound.is_ok(), "привязка UDP к порту {port}: {bound:?}");
        Self { socket, peer: None }
    }
}

/// Почему [`UdpLink`] потерял датаграмму.
#[derive(Debug)]
pub enum UdpError {
    /// Датаграмма длиннее буфера.
    Truncated,
    /// Отвечать некому: датаграмм ещё не было.
    NoPeer,
    /// Стек не отправил датаграмму.
    Send(SendError),
}

impl FrameLink for UdpLink {
    type Error = UdpError;

    async fn recv(&mut self, buf: &mut [u8]) -> Result<usize, LinkError<UdpError>> {
        match self.socket.recv_from(buf).await {
            Ok((len, meta)) => {
                self.peer = Some(meta);
                Ok(len)
            }
            Err(RecvError::Truncated) => Err(LinkError::Lost(UdpError::Truncated)),
        }
    }

    async fn send(&mut self, frame: &[u8]) -> Result<(), LinkError<UdpError>> {
        let Some(peer) = self.peer else {
            return Err(LinkError::Lost(UdpError::NoPeer));
        };
        self.socket
            .send_to(frame, peer)
            .await
            .map_err(|err| LinkError::Lost(UdpError::Send(err)))
    }
}
