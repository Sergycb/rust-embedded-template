//! Соединение с брокером: адрес, TCP-клиент с паузами между попытками и одна
//! сессия `minimq` над готовым потоком байтов.

use core::net::SocketAddrV4;

use defmt_or_log::{FormatOrDebug, info, warn};
use embassy_net::Stack;
use embassy_net::tcp::{self, State, TcpSocket};
use embassy_time::{Duration, Instant, Timer, with_deadline, with_timeout};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use heapless::String;
use minimq::{
    Buffers, ConfigBuilder, ConfigError, Connection, Error as MqttError, PeerError, PubError,
    Publication, QoS, RetainHandling, Session, SubscriptionOptions, TopicFilter, Will,
};

use super::telemetry::{TELEMETRY_INTERVAL, write_telemetry};
use super::topics::{CLIENT_ID_LEN, Command, TOPIC_LEN, Topics, parse_command};
use crate::device::{DeviceInfo, INFO_TEXT};
// {%- if ota == "true" %}
use crate::ota::Client;
use crate::ota::commands::Progress;
// {%- endif %}

/// Как часто проверять молчащего брокера (keep-alive TCP).
const KEEP_ALIVE: Duration = Duration::from_secs(10);

/// Через сколько без ответа на keep-alive и данные соединение считается
/// пропавшим: три проверки подряд.
const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Сколько должно прожить соединение, чтобы отказы до него забылись.
const STABLE: Duration = Duration::from_secs(30);

/// Потолок паузы между попытками.
const MAX_BACKOFF: Duration = Duration::from_secs(30);

/// Буфер входящих пакетов `minimq`: самая длинная команда с темой.
pub const RX_BUFFER: usize = 512;
/// Буфер исходящих пакетов `minimq`: `CONNECT` с Will и входом (его место
/// `minimq` держит всю сессию) и неподтверждённые пакеты `QoS` 1 — подписка,
/// `status`, `info`, исход `reboot`. Сумму держит `const`-assert у `TX_BUDGET`.
pub const TX_BUFFER: usize = 1280;

/// Самые длинные имя и пароль на брокере вместе, байт: они едут в `CONNECT`,
/// а его место занято в [`TX_BUFFER`] всю сессию. Тот же предел держит разбор
/// `mqtt.toml`.
pub const AUTH_MAX: usize = 256;

/// Сколько ждать CONNACK: брокер, который принял TCP, но молчит на `CONNECT`,
/// иначе держал бы узел вечно и без строки в логе.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Сколько `minimq` держит в [`TX_BUFFER`] в худший момент сессии (кодирование —
/// `minimq` 0.13: `handshake.rs`, `outbound.rs`). `CONNECT`: запас под заголовок
/// (5), имя протокола, версия, флаги, keep-alive, три свойства (13 байт и
/// длина), идентификатор, Will `offline` на `status` и вход. Дальше — всё, что
/// может разом ждать подтверждения: подписка, `status`, `info` и исход `reboot`;
/// последний `minimq` кодирует с запасом в 5 байт под заголовок, а не с его
/// длиной.
pub(super) const TX_BUDGET: usize = 5
    + (2 + "MQTT".len() + 1 + 1 + 2)
    + (1 + 13)
    + (2 + CLIENT_ID_LEN)
    + (1 + 2 + CLIENT_ID_LEN + "/status".len() + 2 + "offline".len())
    + (2 + 2 + AUTH_MAX)
    + packet_len(2 + 1 + 2 + CLIENT_ID_LEN + "/cmd/#".len() + 1)
    + packet_len(publish_rest(
        CLIENT_ID_LEN + "/status".len(),
        "online".len(),
    ))
    + packet_len(publish_rest(CLIENT_ID_LEN + "/info".len(), INFO_TEXT))
    + 5
    + publish_rest(TOPIC_LEN, "accepted".len());
const _: () = assert!(
    TX_BUDGET <= TX_BUFFER,
    "CONNECT и неподтверждённые пакеты не влезают в TX_BUFFER"
);

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

/// TCP-клиент — исходящее соединение для служб, которые сами подключаются к
/// серверу (клиент MQTT).
///
/// Отказ сервера он пережидает сам: [`dial`](Self::dial) — одна попытка, и
/// перед ней — пауза по числу отказов подряд, 0, 1, 2, 4 … до 30 с. Отказ —
/// неудачная попытка и соединение, прожившее меньше 30 с до следующего `dial`
/// (сервер оборвал его сразу — неверный пароль, — и бить в него без паузы
/// нельзя); соединение, прожившее дольше, счёт сбрасывает. Чтение и запись без
/// соединения — ошибка: поток принадлежит соединению, которое открыл `dial`.
/// Пропавший сервер снимают keep-alive и таймаут.
pub struct Tcp {
    socket: TcpSocket<'static>,
    /// Отказов подряд — от него пауза перед следующей попыткой.
    failures: u32,
    /// Когда открыто нынешнее соединение (`None` — его нет).
    connected_at: Option<Instant>,
}

// Вручную: у сокетов `embassy-net` нет `Debug`.
impl core::fmt::Debug for Tcp {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Tcp")
            .field("failures", &self.failures)
            .finish_non_exhaustive()
    }
}

impl Tcp {
    /// Клиент на стеке `stack` с буферами сокета `rx` и `tx`.
    #[must_use]
    pub fn new(stack: Stack<'static>, rx: &'static mut [u8], tx: &'static mut [u8]) -> Self {
        Self {
            socket: TcpSocket::new(stack, rx, tx),
            failures: 0,
            connected_at: None,
        }
    }

    /// Выдерживает паузу по числу отказов подряд и один раз пытается
    /// подключиться к `to`; прежнее соединение, если трубку не положили,
    /// закрывается.
    ///
    /// Узел после отказа и после любой ошибки сессии пишет в лог, кладёт
    /// трубку и сразу зовёт `dial` снова. Без паузы он крутился бы, не отдавая исполнитель, и засыпал бы
    /// брокер попытками.
    ///
    /// # Errors
    ///
    /// Подключиться не удалось: сервер не отвечает, отверг соединение или
    /// до него нет маршрута.
    pub async fn dial(&mut self, to: SocketAddrV4) -> Result<(), tcp::ConnectError> {
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

    /// Закрывает текущее соединение (сбросом, без обмена остатком данных);
    /// следующий [`dial`](Self::dial) откроет новое.
    pub fn hang_up(&mut self) {
        // RST уйдёт с ближайшим опросом стека; `dial` его дождётся.
        self.socket.abort();
    }
}

impl ErrorType for Tcp {
    type Error = Error;
}

impl Read for Tcp {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Error> {
        if !is_open(&self.socket) {
            return Err(Error::NotConnected);
        }
        self.socket.read(buf).await.map_err(Error::Io)
    }
}

impl Write for Tcp {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Error> {
        if !is_open(&self.socket) {
            return Err(Error::NotConnected);
        }
        self.socket.write(buf).await.map_err(Error::Io)
    }

    async fn flush(&mut self) -> Result<(), Error> {
        if !is_open(&self.socket) {
            return Err(Error::NotConnected);
        }
        self.socket.flush().await.map_err(Error::Io)
    }
}

/// Почему [`Tcp`] не прочитал или не записал.
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Error {
    /// Соединение оборвалось; следующий `dial` откроет новое.
    Io(tcp::Error),
    /// Чтение или запись без соединения.
    NotConnected,
}

impl core::fmt::Display for Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Io(err) => write!(f, "соединение оборвалось: {err:?}"),
            Self::NotConnected => f.write_str("нет соединения"),
        }
    }
}

impl core::error::Error for Error {}

impl embedded_io_async::Error for Error {
    fn kind(&self) -> ErrorKind {
        match self {
            Self::Io(_) => ErrorKind::ConnectionReset,
            Self::NotConnected => ErrorKind::NotConnected,
        }
    }
}

/// Чем кончилась сессия. Строку лога об этом пишет сама [`serve`]: узлу
/// после любого исхода — положить трубку и подключиться снова.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Ended {
    /// Брокер не принял подключение или не ответил на него за [`CONNECT_TIMEOUT`].
    Refused,
    /// Плата была в сети, и связь пропала.
    Lost,
}

/// Что нужно сессии, кроме потока и адреса брокера.
pub(crate) struct Env<'a> {
    /// Темы этой платы.
    pub topics: &'a Topics,
    /// Сведения для `info`.
    pub info: &'a DeviceInfo,
    /// MAC для `info`.
    pub mac: [u8; 6],
    /// Адрес платы для `info`; `None` — ещё не получен.
    pub ip: Option<embassy_net::Ipv4Address>,
    /// Буфер входящих пакетов, [`RX_BUFFER`].
    pub rx: &'a mut [u8],
    /// Буфер исходящих пакетов, [`TX_BUFFER`].
    pub tx: &'a mut [u8],
    // {%- if ota == "true" %}
    /// Клиент входа `CMD` узла OTA — для `cmd/reboot`.
    pub ota: &'a Client,
    // {%- endif %}
}

/// Пауза перед попыткой после `failures` отказов подряд: 0, 1, 2, 4 … с, не
/// больше [`MAX_BACKOFF`].
fn backoff(failures: u32) -> Duration {
    match failures {
        0 => Duration::from_secs(0),
        // Сдвиг ограничен: дальше 2⁵ с пауза всё равно упирается в потолок.
        n => Duration::from_secs(1 << (n - 1).min(5)).min(MAX_BACKOFF),
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

/// Одна сессия с брокером над установленным соединением `io`: подключение с
/// Will, подписка на команды, `status` и `info`, дальше телеметрия и команды,
/// пока связь жива. Паузы между попытками — не здесь: их держит транспорт.
///
/// Допущение: Receive Maximum брокера (CONNACK) — не меньше 3. Столько
/// публикаций `QoS` 1 сессия держит неподтверждёнными разом — `status`, `info`
/// и исход `reboot`, те же, что в `TX_BUDGET`; Will в счёт не идёт, его
/// публикует брокер. Квоту `minimq` 0.13 берёт из CONNACK и на исчерпанной
/// `publish` отвечает `NotReady`, а не ждёт PUBACK, — сессия сочтёт это
/// обрывом и переподключится. Брокеры по умолчанию дают больше
/// (у Mosquitto — 20, `max_inflight_messages`).
pub(crate) async fn serve<IO: minimq::Io>(io: IO, endpoint: &Broker, env: &mut Env<'_>) -> Ended
where
    IO::Error: FormatOrDebug,
{
    let [a, b, c, d] = endpoint.addr;
    let port = endpoint.port;
    let Env {
        topics,
        info,
        mac,
        ip,
        rx,
        tx,
        // {%- if ota == "true" %}
        ota,
        // {%- endif %}
    } = env;
    let status = topics.status();
    let Ok(config) = config(endpoint, topics, &status, rx, tx) else {
        warn!("mqtt: настройки клиента не приняты");
        return Ended::Refused;
    };
    let mut session = Session::new(config);
    // Отмена по таймауту безопасна: сессия после неё выбрасывается, сокет
    // закрывает `hang_up` узла.
    let mut conn = match with_timeout(CONNECT_TIMEOUT, session.connect(io)).await {
        Ok(Ok(conn)) => conn,
        Ok(Err(err)) => {
            warn!(
                "mqtt: брокер {}.{}.{}.{}:{} не принял подключение ({:?}) — проверьте mqtt.toml",
                a, b, c, d, port, err
            );
            return Ended::Refused;
        }
        Err(_) => {
            warn!(
                "mqtt: брокер {}.{}.{}.{}:{} не ответил на CONNECT — проверьте mqtt.toml",
                a, b, c, d, port
            );
            return Ended::Refused;
        }
    };
    info!("mqtt: подключён к брокеру {}.{}.{}.{}:{}", a, b, c, d, port);

    // Ошибка `minimq` после подключения — связь потеряна: соединение больше
    // не годится, следующая сессия — с новым. Кроме отказа брокера в ответ на
    // подписку или публикацию (ниже, у `recv`).
    let online = async {
        announce(&mut conn, topics, &status, info, *mac, *ip).await?;
        let mut next = Instant::now() + TELEMETRY_INTERVAL;
        loop {
            let command = match with_deadline(next, conn.recv()).await {
                // `recv` отменяется без потерь; публикация `QoS` 0 — нет, она
                // вне таймаута.
                Err(_) => {
                    let mut text = String::<40>::new();
                    write_telemetry(&mut text, Instant::now().as_secs())
                        .expect("u64 в тексте влезает в 40 байт");
                    let topic = topics.telemetry();
                    conn.publish(Publication::text(&topic, &text))
                        .await
                        .map_err(session_error)?;
                    next = Instant::now() + TELEMETRY_INTERVAL;
                    continue;
                }
                // SUBACK или PUBACK с кодом отказа (чаще всего — ACL брокера
                // не пускает в тему): `minimq` отдаёт его из `recv`, а
                // соединение живо — обрыв здесь крутил бы переподключения, не
                // назвав причины.
                Ok(Err(MqttError::Peer(PeerError::Rejected(code)))) => {
                    warn!("mqtt: брокер отказал: {:?}", code);
                    continue;
                }
                Ok(Err(err)) => return Err(err),
                Ok(Ok(message)) if message.retained() => {
                    warn!("mqtt: команда {} с retain пропущена", message.topic());
                    continue;
                }
                Ok(Ok(message)) => {
                    let Some(command) = parse_command(topics, message.topic()) else {
                        info!("mqtt: неизвестная команда {}", message.topic());
                        continue;
                    };
                    command
                }
            };
            match command {
                Command::Reboot => {
                    // {%- if ota == "true" %}
                    let reply = ota.request(crate::ota::Command::Reboot).await;
                    let text = if matches!(reply, Ok(Ok(Progress::Rebooting))) {
                        "accepted"
                    } else {
                        "refused"
                    };
                    let topic = topics.reboot_result();
                    conn.publish(Publication::text(&topic, text).qos(QoS::AtLeastOnce))
                        .await
                        .map_err(session_error)?;
                    // {%- endif %}
                    // {%- if ota != "true" %}
                    warn!("mqtt: перезагрузка без OTA недоступна");
                    // {%- endif %}
                }
            }
        }
    };
    let Err(err): Result<core::convert::Infallible, MqttError<IO::Error>> = online.await;
    warn!(
        "mqtt: связь с брокером {}.{}.{}.{}:{} пропала: {:?}",
        a, b, c, d, port, err
    );
    Ended::Lost
}

/// Ошибка сессии из ошибки публикации. Тело у сессии — готовый текст, и
/// отказа его сериализации не бывает; случись он — это неверный запрос.
fn session_error<P, E>(err: PubError<P, E>) -> MqttError<E> {
    match err {
        PubError::Session(err) => err,
        PubError::Payload(_) => MqttError::InvalidRequest,
    }
}

/// Длина пакета с оставшейся частью `rest` байт (меньше 16384): тип и длина
/// varint.
const fn packet_len(rest: usize) -> usize {
    1 + if rest < 128 { 1 } else { 2 } + rest
}

/// Оставшаяся часть PUBLISH `QoS` 1 без свойств: тема, номер пакета, пустые
/// свойства, тело.
const fn publish_rest(topic: usize, payload: usize) -> usize {
    2 + topic + 2 + 1 + payload
}

/// Настройки клиента: идентификатор, вход (при непустом `user`) и Will
/// `offline` на `status`.
fn config<'a>(
    endpoint: &Broker,
    topics: &Topics,
    status: &str,
    rx: &'a mut [u8],
    tx: &'a mut [u8],
) -> Result<ConfigBuilder<'a>, ConfigError> {
    let mut config = ConfigBuilder::new(Buffers::new(rx, tx)).client_id(&topics.client_id())?;
    if !endpoint.user.is_empty() {
        config = config.auth(endpoint.user, endpoint.password.as_bytes())?;
    }
    config.will(
        Will::new(status, b"offline", &[])?
            .retained()
            .qos(QoS::AtLeastOnce),
    )
}

/// Начало сессии: подписка на команды, `status` = `online` и `info`.
async fn announce<IO: minimq::Io>(
    conn: &mut Connection<'_, '_, IO>,
    topics: &Topics,
    status: &str,
    info: &DeviceInfo,
    mac: [u8; 6],
    ip: Option<embassy_net::Ipv4Address>,
) -> Result<(), MqttError<IO::Error>> {
    let filter = topics.cmd_filter();
    let options = SubscriptionOptions::default()
        // Команда, оставленная с retain, иначе приходила бы при каждом
        // подключении.
        .retain_behavior(RetainHandling::Never)
        // Свой `cmd/reboot/result` не возвращается «неизвестной командой».
        .ignore_local_messages()
        .maximum_qos(QoS::AtLeastOnce);
    conn.subscribe(&[TopicFilter::new(&filter).options(options)], &[])
        .await?;
    // `online` — каждый раз: брокер мог уже разослать Will `offline`.
    conn.publish(
        Publication::text(status, "online")
            .qos(QoS::AtLeastOnce)
            .retain(),
    )
    .await
    .map_err(session_error)?;
    let mut text = String::<INFO_TEXT>::new();
    if crate::device::write_info(
        &mut text,
        info,
        Some(mac),
        ip.map(|ip| core::net::Ipv4Addr::from(ip.octets())),
        Instant::now().as_secs(),
    )
    .is_err()
    {
        warn!("mqtt: сведения об устройстве не влезли в буфер");
        return Ok(());
    }
    let topic = topics.info();
    conn.publish(
        Publication::text(&topic, &text)
            .qos(QoS::AtLeastOnce)
            .retain(),
    )
    .await
    .map_err(session_error)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! Тесты TCP-клиента: паузы между попытками и таймаут молчащего адреса.

    use core::cell::Cell;

    use embassy_net::tcp::TcpSocket;
    use embassy_time::MockDriver;
    use embedded_io_async::{Read, Write};

    use adapters_hw::net::fake::cable::{HOST, Loopback, leak, pair};
    use embassy_net::Runner;

    use super::*;

    /// Порт сервера на «хосте» пары.
    const PORT: u16 = 1883;

    /// Сервер на «хосте» пары, к которому подключается [`Tcp`].
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
        let mut client = Tcp::new(device, leak([0; 64]), leak([0; 64]));
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

    /// Гоняет оба стека, пока идёт `test`.
    async fn with_network<R>(
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
        let mut client = Tcp::new(device, leak([0; 64]), leak([0; 64]));
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
        let mut client = Tcp::new(device, leak([0; 64]), leak([0; 64]));
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
        let mut client = Tcp::new(device, leak([0; 64]), leak([0; 64]));
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
        let mut client = Tcp::new(device, leak([0; 64]), leak([0; 64]));
        assert!(matches!(
            client.read(&mut [0; 4]).await,
            Err(Error::NotConnected)
        ));
        assert!(matches!(client.write(b"x").await, Err(Error::NotConnected)));
        assert!(matches!(client.flush().await, Err(Error::NotConnected)));
    }
}
