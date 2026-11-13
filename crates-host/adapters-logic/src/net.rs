//! Транспорты протоколов над сокетами `embassy-net`:
// {%- if has_tcp == "true" %}
//! TCP-сервер — поток байтов ([`Server`](crate::net::Server));
// {%- endif %}
// {%- if has_udp == "true" %}
//! UDP-канал — кадр в датаграмме ([`UdpLink`](crate::net::UdpLink));
// {%- endif %}
//! стек строит плата (`bsp::net`), транспорты на нём — compose-site.
//!
//! Подмодули — внутри файла: отдельных файлов под них нет и при одном
//! транспорте тоже. Оба объявлены здесь одним файлом: в репозитории шаблона
//! крейт компилируется целиком, а генерация вырезает невыбранный транспорт
//! Liquid-веткой.

// {%- if has_tcp == "true" %}
mod tcp {
    //! TCP-сервер на одном порту — поток байтов для транспортов протоколов.

    use embassy_net::Stack;
    use embassy_net::tcp::{self, AcceptError, State, TcpSocket};
    use embassy_time::Duration;
    use embedded_io_async::{ErrorKind, ErrorType, Read, Write};

    /// Как часто проверять молчащего собеседника (keep-alive TCP).
    const KEEP_ALIVE: Duration = Duration::from_secs(10);

    /// Через сколько без ответа на keep-alive и данные соединение считается
    /// пропавшим: три проверки подряд.
    const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

    /// TCP-сервер на одном порту — поток байтов для транспортов протоколов.
    // {%- if modbus_tcp == "true" %}
    /// Над ним работает `modbus::Tcp`.
    // {%- endif %}
    // {%- if cobs_used == "true" %}
    /// Над ним работает `cobs::Cobs`.
    // {%- endif %}
    ///
    /// Соединение — его забота, а не протокола: чтение ждёт клиента (`accept`),
    /// а соединение, закрытое клиентом или оборванное, сбрасывается — протокол
    /// видит конец потока, и следующее чтение принимает нового клиента. Запись
    /// без соединения — ошибка, а не ожидание клиента: ответ ушедшему клиенту
    /// не должен достаться следующему. Клиент один: пока подключён первый,
    /// второму стек отвечает RST (соединение отвергнуто), и переподключаться ему —
    /// после ухода первого. Молчаливо пропавшего клиента (обесточен, вынут кабель)
    /// снимают keep-alive и таймаут соединения — иначе сокет занят навсегда.
    pub struct Server {
        socket: TcpSocket<'static>,
        port: u16,
    }

    // Вручную: у сокетов `embassy-net` нет `Debug`.
    impl core::fmt::Debug for Server {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("Server")
                .field("port", &self.port)
                .finish_non_exhaustive()
        }
    }

    impl Server {
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
        async fn connected(&mut self) -> Result<(), Error> {
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
            self.socket.accept(self.port).await.map_err(Error::Accept)
        }

        /// Сбрасывает соединение: следующее обращение примет новое.
        async fn drop_connection(&mut self) {
            reset(&mut self.socket).await;
        }
        // {%- if modbus_tcp == "true" %}

        /// Обрывает соединение: следующее чтение ждёт нового клиента. Нужен
        /// протоколу, который, потеряв в потоке границу кадра, в том же
        /// соединении её уже не найдёт (`modbus::Tcp`): клиент переподключится и
        /// начнёт с чистого потока.
        pub(crate) async fn hang_up(&mut self) {
            self.drop_connection().await;
        }
        // {%- endif %}
    }
    impl ErrorType for Server {
        type Error = Error;
    }

    impl Read for Server {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
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
                    Err(Error::Io(err))
                }
            }
        }
    }

    impl Write for Server {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
            if !self.is_connected() {
                return Err(Error::NotConnected);
            }
            match self.socket.write(buf).await {
                Ok(n) => Ok(n),
                Err(err) => {
                    self.drop_connection().await;
                    Err(Error::Io(err))
                }
            }
        }

        async fn flush(&mut self) -> Result<(), Error> {
            if !self.is_connected() {
                return Err(Error::NotConnected);
            }
            match self.socket.flush().await {
                Ok(()) => Ok(()),
                Err(err) => {
                    self.drop_connection().await;
                    Err(Error::Io(err))
                }
            }
        }
    }

    /// Почему [`Server`] не прочитал или не записал.
    #[derive(Debug)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
    pub enum Error {
        /// Клиента не удалось принять.
        Accept(AcceptError),
        /// Соединение оборвалось; следующее чтение примет новое.
        Io(tcp::Error),
        /// Запись без соединения: клиент, которому шёл ответ, уже отключился.
        NotConnected,
    }

    impl core::fmt::Display for Error {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            match self {
                Self::Accept(err) => write!(f, "клиент не принят: {err:?}"),
                Self::Io(err) => write!(f, "соединение оборвалось: {err:?}"),
                Self::NotConnected => f.write_str("нет соединения"),
            }
        }
    }

    impl core::error::Error for Error {}

    impl embedded_io_async::Error for Error {
        fn kind(&self) -> ErrorKind {
            match self {
                // Ошибка программы, а не сети: порт 0 или сокет в чужом состоянии.
                Self::Accept(AcceptError::InvalidPort | AcceptError::InvalidState) => {
                    ErrorKind::Other
                }
                Self::Accept(AcceptError::ConnectionReset) | Self::Io(_) => {
                    ErrorKind::ConnectionReset
                }
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

    #[cfg(test)]
    mod tests {
        use adapters_hw::net::fake::cable::{DEVICE, leak, pair};
        use embassy_net::tcp::{self, AcceptError, State, TcpSocket};
        use embedded_io_async::{ErrorKind, Read, Write};

        use super::*;
        use crate::net::fake::{PORT, with_network};

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
            let mut server = Server::new(device, PORT, leak([0; 256]), leak([0; 256]));
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
            let mut server = Server::new(device, PORT, leak([0; 64]), leak([0; 64]));
            with_network(device_runner, host_runner, async {
                let device_side = async {
                    let mut buf = [0; 8];
                    assert!(matches!(
                        server.read(&mut buf).await,
                        Err(Error::Io(tcp::Error::ConnectionReset))
                    ));
                    assert_eq!(server.socket.state(), State::Closed);
                    assert!(!server.is_connected());
                    assert!(matches!(
                        server.write(b"late").await,
                        Err(Error::NotConnected)
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
            let mut server = Server::new(device, PORT, leak([0; 64]), leak([0; 64]));
            assert!(matches!(
                server.write(b"late").await,
                Err(Error::NotConnected)
            ));
            assert!(matches!(server.flush().await, Err(Error::NotConnected)));
            assert_eq!(server.read(&mut []).await.unwrap(), 0);
            assert_eq!(
                embedded_io_async::Error::kind(&Error::NotConnected),
                ErrorKind::NotConnected
            );
            assert_eq!(
                embedded_io_async::Error::kind(&Error::Accept(AcceptError::InvalidPort)),
                ErrorKind::Other
            );
            assert_eq!(
                embedded_io_async::Error::kind(&Error::Io(tcp::Error::ConnectionReset)),
                ErrorKind::ConnectionReset
            );
        }
    }
}
pub use tcp::{Error, Server};
// {%- endif %}

// {%- if has_udp == "true" %}
mod udp {
    //! UDP-транспорт: [`UdpLink`] и его ошибка.

    use core::mem::MaybeUninit;

    use embassy_net::Stack;
    use embassy_net::udp::{PacketMetadata, RecvError, SendError, UdpMetadata, UdpSocket};
    use ports::{FrameDuplex, FrameLink, LinkError};

    /// UDP-канал на одном порту — [`FrameLink`] для протоколов, которым
    /// датаграмма и есть кадр.
    // {%- if modbus_udp == "true" %}
    /// Так работает `modbus::Udp`.
    // {%- endif %}
    // {%- if has_protobuf == "true" %}
    /// Так работает узел PROTOBUF.
    // {%- endif %}
    ///
    /// Отвечает тому, кто прислал последнюю датаграмму: адрес отправителя порт
    /// не передаёт, а протоколы отвечают на запрос.
    pub struct UdpLink {
        socket: UdpSocket<'static>,
        peer: Option<UdpMetadata>,
        /// Буфер канала: принятая датаграмма и собираемая к отправке.
        buf: &'static mut [u8],
    }

    // Вручную: у сокетов `embassy-net` нет `Debug`.
    impl core::fmt::Debug for UdpLink {
        fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
            f.debug_struct("UdpLink").finish_non_exhaustive()
        }
    }

    impl UdpLink {
        /// Канал на порту `port` стека `stack`: буферы приёма и передачи сокета и
        /// память под их метаданные — по записи на датаграмму в очереди, `Q` в
        /// каждую сторону; `buf` — буфер канала (`FrameLink::buffer`), не короче
        /// самой длинной датаграммы протокола.
        ///
        /// Метаданные приходят неинициализированными (`MaybeUninit`) и
        /// заполняются здесь: память от compose-site — нули (`static T` графа), а
        /// нулевые байты — не `PacketMetadata`.
        ///
        /// # Panics
        ///
        /// Если `port` 0 — сокет без порта не принимает ничего.
        #[must_use]
        pub fn new<const Q: usize>(
            stack: Stack<'static>,
            port: u16,
            rx_meta: &'static mut MaybeUninit<[PacketMetadata; Q]>,
            rx: &'static mut [u8],
            tx_meta: &'static mut MaybeUninit<[PacketMetadata; Q]>,
            tx: &'static mut [u8],
            buf: &'static mut [u8],
        ) -> Self {
            assert!(port != 0, "UDP-канал на порту 0");
            let rx_meta = rx_meta.write([PacketMetadata::EMPTY; Q]);
            let tx_meta = tx_meta.write([PacketMetadata::EMPTY; Q]);
            let mut socket = UdpSocket::new(stack, rx_meta, rx, tx_meta, tx);
            // Свежий сокет с ненулевым портом привязывается всегда.
            let bound = socket.bind(port);
            assert!(bound.is_ok(), "привязка UDP к порту {port}: {bound:?}");
            Self {
                socket,
                peer: None,
                buf,
            }
        }
    }

    /// Почему [`UdpLink`] потерял датаграмму.
    #[derive(Debug)]
    #[cfg_attr(feature = "defmt", derive(defmt::Format))]
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

        async fn recv(&mut self) -> Result<usize, LinkError<UdpError>> {
            match self.socket.recv_from(self.buf).await {
                Ok((len, meta)) => {
                    self.peer = Some(meta);
                    Ok(len)
                }
                Err(RecvError::Truncated) => Err(LinkError::Lost(UdpError::Truncated)),
            }
        }

        fn buffer(&mut self) -> &mut [u8] {
            self.buf
        }

        async fn send(&mut self, len: usize) -> Result<(), LinkError<UdpError>> {
            let Self { socket, peer, buf } = self;
            send_to(socket, *peer, &buf[..len]).await
        }
    }

    /// Датаграмма снимается с сокета в том же опросе, что её возвращает
    /// (`UdpSocket::recv_from`), — брошенный `recv` её не теряет.
    impl FrameDuplex for UdpLink {
        async fn send_from(&mut self, frame: &[u8]) -> Result<(), LinkError<UdpError>> {
            send_to(&mut self.socket, self.peer, frame).await
        }
    }

    /// Датаграмма `frame` — отправителю последней принятой.
    async fn send_to(
        socket: &mut UdpSocket<'static>,
        peer: Option<UdpMetadata>,
        frame: &[u8],
    ) -> Result<(), LinkError<UdpError>> {
        let Some(peer) = peer else {
            return Err(LinkError::Lost(UdpError::NoPeer));
        };
        socket
            .send_to(frame, peer)
            .await
            .map_err(|err| LinkError::Lost(UdpError::Send(err)))
    }

    #[cfg(test)]
    mod tests {
        use adapters_hw::net::fake::cable::{DEVICE, leak, pair};

        use super::*;
        use crate::net::fake::{PORT, with_network};

        fn udp(stack: Stack<'static>, port: u16) -> UdpLink {
            UdpLink::new(
                stack,
                port,
                leak(MaybeUninit::<[PacketMetadata; 4]>::uninit()),
                leak([0; 16]),
                leak(MaybeUninit::<[PacketMetadata; 4]>::uninit()),
                leak([0; 64]),
                leak([0; 8]),
            )
        }

        /// UDP-канал отвечает отправителю последней датаграммы; ответить до
        /// первой датаграммы некому, длинная датаграмма — потеря.
        #[tokio::test]
        async fn udp_link_replies_to_the_last_sender() {
            let ((device, device_runner), (host, host_runner)) = pair();
            let mut link = udp(device, PORT);
            let peer = udp(host, 4000);
            with_network(device_runner, host_runner, async {
                assert!(matches!(
                    link.send(1).await,
                    Err(LinkError::Lost(UdpError::NoPeer))
                ));

                peer.socket
                    .send_to(&[1, 2, 3], (DEVICE, PORT))
                    .await
                    .unwrap();
                assert_eq!(link.recv().await.unwrap(), 3);
                assert_eq!(link.buffer()[..3], [1, 2, 3]);
                link.buffer()[..2].copy_from_slice(&[9, 8]);
                link.send(2).await.unwrap();
                let mut reply = [0; 16];
                let (n, from) = peer.socket.recv_from(&mut reply).await.unwrap();
                assert_eq!((&reply[..n], from.endpoint.port), (&[9, 8][..], PORT));

                peer.socket.send_to(&[0; 12], (DEVICE, PORT)).await.unwrap();
                // Датаграмма длиннее буфера канала (8 байт).
                assert!(matches!(
                    link.recv().await,
                    Err(LinkError::Lost(UdpError::Truncated))
                ));
            })
            .await;
        }
    }
}
pub use udp::{UdpError, UdpLink};
// {%- endif %}

#[cfg(test)]
pub(crate) mod fake {
    //! Поверх кабеля в памяти (`adapters_hw::net::fake::cable`) — то, что нужно тестам
    //! транспортов: порт и прогон обоих стеков.

    use adapters_hw::net::fake::cable::Loopback;
    use embassy_net::Runner;

    pub(crate) const PORT: u16 = 502;

    /// Гоняет оба стека, пока идёт `test`.
    pub(crate) async fn with_network<R>(
        mut device: Runner<'static, Loopback>,
        mut host: Runner<'static, Loopback>,
        test: impl Future<Output = R>,
    ) -> R {
        tokio::select! {
            never = device.run() => never,
            never = host.run() => never,
            result = test => result,
        }
    }
}
