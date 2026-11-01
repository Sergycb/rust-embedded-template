//! Клиент устройства для сценариев `cargo xtask test host-target`
//! (`tests/test.rs`).
//!
//! Третий этап трёхэтапной модели: хост проверяет готовое устройство только
//! через транспорт и протокол проекта — так, как с ним будет говорить
//! настоящий хост или соседний сервис. Пробник этому этапу не нужен: он лишь
//! необязательно прошивает плату перед тестами (`xtask`), а сами сценарии
//! ничего не читают из памяти и не сбрасывают ядро мимо протокола.
//!
//! Здесь — клиент (подключение, кадры, команды), в `tests/` — сценарии. Адрес
//! устройства — из окружения:
//!
//! * `HOST_TARGET_ADDR` — по сети: `ip` или `ip:порт`, порт по умолчанию —
//!   тот, что у узла протокола в прошивке; приставка `tcp://`/`udp://`
//!   выбирает транспорт, без неё — сетевой транспорт протокола в проекте;
//! * `HOST_TARGET_PORT` — последовательный порт (`COM3`, `/dev/ttyACM0`):
//!   UART через переходник или USB CDC; скорость UART — `HOST_TARGET_BAUD`
//!   (по умолчанию 115200, как в примере подключения UART к плате).
//!
//! Не задан ни один — сценарии пропускаются, и `xtask` пишет это в сводке.
//!
//! Протоколы. Первая итерация — protobuf (схема — крейт `proto`). Сценарии под
//! остальные встанут сюда же, вторым клиентом рядом с этим, а сами сценарии
//! останутся теми же: Modbus (окно OTA в регистрах `domain::modbus` по RTU,
//! TCP, UDP) и REST (`/api/info`) — следующая итерация; CANopen (SDO-домен
//! OTA), MQTT (темы `cmd/`) и SSH — отложены. Протокол `none` — свой: тесты
//! под него пишет проект, поверх своего канала.
// {%- if has_protobuf == "true" %}

use std::{
    env,
    io::{self, ErrorKind},
    net::{SocketAddr, ToSocketAddrs, UdpSocket},
    thread,
    time::{Duration, Instant},
};
// {%- if cobs_used == "true" %}
use std::{
    collections::VecDeque,
    io::{Read, Write},
    net::TcpStream,
};
// {%- endif %}

use micropb::{MessageDecode, MessageEncode, PbEncoder};
use proto::{Get, Request, Request_, Response, Response_, heapless};

/// Порт узла protobuf по сети — `PORT` модулей `link::tcp` и `link::udp` прошивки.
const NET_PORT: u16 = 5000;

/// Сетевой транспорт protobuf в проекте — им идёт `HOST_TARGET_ADDR` без
/// приставки; пусто — сетевого нет.
const NET_KIND: &str =
    "{% if link_kinds contains 'tcp' %}tcp{% elsif link_kinds contains 'udp' %}udp{% endif %}";

/// Сколько ждать ответа на обычную команду.
const REPLY_TIMEOUT: Duration = Duration::from_secs(5);

// {%- if ota == "true" %}
/// Пауза после ответа на `Reboot`, прежде чем звать устройство снова: узел
/// сбрасывает МК через `REBOOT_GRACE` (100 мс) после ответа, и прежняя
/// прошивка не должна успеть ответить за новую.
const RESET_PAUSE: Duration = Duration::from_secs(1);
// {%- endif %}
// {%- if protobuf_serial == "true" %}

/// Скорость UART по умолчанию — как в примере подключения UART к плате.
/// USB CDC её не замечает.
const DEFAULT_BAUD: u32 = 115_200;
// {%- endif %}
// {%- if cobs_used == "true" %}

/// CRC кадра — та же, что у `adapters::cobs` в прошивке.
const CRC: crc::Crc<u16> = crc::Crc::<u16>::new(&crc::CRC_16_IBM_SDLC);
// {%- endif %}

/// Что устройство говорит о себе (`Info`) — то, что смотрят сценарии.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    /// Версия прошивки текстом.
    pub version: String,
    /// Имя чипа.
    pub chip: String,
    /// Причина последнего сброса (`software`, `pin`…).
    pub reset: String,
}

/// Исход команды: `Ok` или отказ устройства — текст его `Error` (имя отказа
/// узла OTA: `Signature`, `Rollback`, `Busy`…).
pub type Outcome = Result<(), String>;

/// Канал до устройства: отправить сообщение, принять следующее.
trait Channel {
    fn send(&mut self, message: &[u8]) -> io::Result<()>;
    /// Следующее сообщение до `deadline`; не пришло — `TimedOut`.
    fn recv(&mut self, deadline: Instant) -> io::Result<Vec<u8>>;
}

/// Куда подключаться.
#[derive(Debug, Clone)]
enum Target {
    // {%- if cobs_used == "true" %}
    /// TCP: поток кадров COBS + CRC.
    Tcp(SocketAddr),
    // {%- endif %}
    /// UDP: датаграмма — сообщение.
    Udp(SocketAddr),
    // {%- if protobuf_serial == "true" %}
    /// Последовательный порт: поток кадров COBS + CRC.
    Serial { port: String, baud: u32 },
    // {%- endif %}
}

/// Устройство на связи — по адресу из окружения.
///
/// Подключение ленивое и восстанавливается само: порвался канал (сброс
/// устройства рвёт TCP и уносит порт USB CDC) — следующий вызов подключается
/// заново.
pub struct Device {
    target: Target,
    channel: Option<Box<dyn Channel>>,
    next_id: u32,
}

// {%- if cobs_used == "true" %}
/// Поток кадров COBS + CRC: на проводе `0x00`, COBS от `[сообщение][CRC LE]`,
/// `0x00` — как пишет и читает `adapters::cobs` в прошивке.
struct Framed<W> {
    wire: W,
    /// Прочитано, но не разобрано: хвост за концом кадра — начало следующего.
    inbox: VecDeque<u8>,
    /// Собираемый кадр.
    frame: Vec<u8>,
}

impl<W: Wire> Framed<W> {
    fn new(wire: W) -> Self {
        Self {
            wire,
            inbox: VecDeque::new(),
            frame: Vec::new(),
        }
    }

    /// Сообщение из кадра; `None` — кадр битый (COBS или CRC), его
    /// пропускаем, как и прошивка.
    fn unframe(frame: &[u8]) -> Option<Vec<u8>> {
        let mut body = cobs::decode_vec(frame).ok()?;
        let at = body.len().checked_sub(2)?;
        let crc = u16::from_le_bytes([body[at], body[at + 1]]);
        body.truncate(at);
        (CRC.checksum(&body) == crc).then_some(body)
    }
}

impl<W: Wire> Channel for Framed<W> {
    fn send(&mut self, message: &[u8]) -> io::Result<()> {
        let mut body = message.to_vec();
        body.extend_from_slice(&CRC.checksum(message).to_le_bytes());
        let mut frame = vec![0];
        frame.extend(cobs::encode_vec(&body));
        frame.push(0);
        self.wire.write_all(&frame)?;
        self.wire.flush()
    }

    fn recv(&mut self, deadline: Instant) -> io::Result<Vec<u8>> {
        loop {
            while let Some(byte) = self.inbox.pop_front() {
                if byte != 0 {
                    self.frame.push(byte);
                    continue;
                }
                let frame = std::mem::take(&mut self.frame);
                // Пустой кадр — два разделителя подряд (начало и конец).
                if !frame.is_empty()
                    && let Some(message) = Self::unframe(&frame)
                {
                    return Ok(message);
                }
            }
            let left = deadline.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return Err(ErrorKind::TimedOut.into());
            }
            self.wire.set_timeout(left)?;
            let mut buf = [0; 512];
            match self.wire.read(&mut buf) {
                Ok(0) => return Err(ErrorKind::ConnectionAborted.into()),
                Ok(n) => self.inbox.extend(&buf[..n]),
                Err(err) if matches!(err.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock) => {}
                Err(err) => return Err(err),
            }
        }
    }
}

/// Поток под [`Framed`]: чтение и запись с пределом ожидания.
trait Wire: Read + Write {
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()>;
}

impl Wire for TcpStream {
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        self.set_read_timeout(Some(timeout))
    }
}
// {%- endif %}
// {%- if protobuf_serial == "true" %}

impl Wire for Box<dyn serialport::SerialPort> {
    fn set_timeout(&mut self, timeout: Duration) -> io::Result<()> {
        serialport::SerialPort::set_timeout(self.as_mut(), timeout).map_err(io::Error::from)
    }
}
// {%- endif %}

/// UDP: датаграмма — сообщение, как у `adapters::net::UdpLink`.
struct Datagram(UdpSocket);

impl Channel for Datagram {
    fn send(&mut self, message: &[u8]) -> io::Result<()> {
        self.0.send(message).map(drop)
    }

    fn recv(&mut self, deadline: Instant) -> io::Result<Vec<u8>> {
        let left = deadline.saturating_duration_since(Instant::now());
        if left.is_zero() {
            return Err(ErrorKind::TimedOut.into());
        }
        self.0.set_read_timeout(Some(left))?;
        let mut buf = [0; 2048];
        match self.0.recv(&mut buf) {
            Ok(n) => Ok(buf[..n].to_vec()),
            Err(err) if err.kind() == ErrorKind::WouldBlock => Err(ErrorKind::TimedOut.into()),
            Err(err) => Err(err),
        }
    }
}

impl Device {
    /// Устройство по адресу из окружения (см. модуль). `None` — адрес не
    /// задан: сценарий пропускается, причина — в stderr (видна с
    /// `--no-capture`) и в сводке `xtask`. Адрес задан, но негодный — паника:
    /// это ошибка запуска, а не повод пропустить проверку.
    ///
    /// # Panics
    ///
    /// Адрес в окружении задан, но негодный.
    #[must_use]
    pub fn from_env() -> Option<Self> {
        let target = match target_from_env() {
            Ok(Some(target)) => target,
            Ok(None) => {
                eprintln!(
                    "адрес устройства не задан (HOST_TARGET_ADDR или HOST_TARGET_PORT) — сценарий \
                     пропущен"
                );
                return None;
            }
            Err(why) => panic!("{why}"),
        };
        Some(Self {
            target,
            channel: None,
            next_id: 1,
        })
    }

    /// Сведения об устройстве.
    ///
    /// # Errors
    ///
    /// Канал не открылся или ответа нет в срок.
    pub fn info(&mut self) -> io::Result<Info> {
        match self.call(Request_::Command::Info(Get::default()), REPLY_TIMEOUT)? {
            Response_::Result::Info(info) => Ok(Info {
                version: info.version.as_str().to_owned(),
                chip: info.chip.as_str().to_owned(),
                reset: info.reset.as_str().to_owned(),
            }),
            other => Err(io::Error::other(format!("на Info пришло {other:?}"))),
        }
    }

    /// Ждёт, пока устройство снова ответит на `Info`, — после сброса, — но не
    /// дольше `within`. Канал открывается заново: сброс рвёт TCP и уносит порт
    /// USB CDC, а UART мог принять мусор.
    ///
    /// # Errors
    ///
    /// Не ответило за `within` — текст последней ошибки.
    pub fn wait_back(&mut self, within: Duration) -> Result<Info, String> {
        let start = Instant::now();
        loop {
            self.channel = None;
            match self.info() {
                Ok(info) => {
                    // Видно с `--no-capture`: так меряется, сколько оставлено запаса.
                    eprintln!(
                        "устройство ответило через {:?} из {within:?}",
                        start.elapsed()
                    );
                    return Ok(info);
                }
                Err(err) if start.elapsed() >= within => {
                    return Err(format!(
                        "не ответило за {within:?}; последняя ошибка: {err}"
                    ));
                }
                Err(_) => thread::sleep(Duration::from_millis(500)),
            }
        }
    }
    // {%- if ota == "true" %}

    /// `Reboot`: ответ приходит до сброса. После `Ok` — пауза, чтобы
    /// следующий вызов не застал прежнюю прошивку.
    ///
    /// # Errors
    ///
    /// Канал не открылся или ответа нет в срок.
    pub fn reboot(&mut self) -> io::Result<Outcome> {
        let outcome = outcome(self.call(
            Request_::Command::Reboot(proto::Reboot::default()),
            REPLY_TIMEOUT,
        )?);
        if outcome.is_ok() {
            self.channel = None;
            thread::sleep(RESET_PAUSE);
        }
        Ok(outcome)
    }

    /// `ota_start`: длина образа и подпись (пустая — без подписи). Ответа
    /// ждёт `slow`: перед ним устройство стирает раздел под образ.
    ///
    /// # Errors
    ///
    /// Канал не открылся, ответа нет в срок или подпись длиннее схемы.
    pub fn start(&mut self, len: usize, signature: &[u8], slow: Duration) -> io::Result<Outcome> {
        let start = proto::OtaStart {
            len: u32::try_from(len).map_err(io::Error::other)?,
            signature: heapless::Vec::from_slice(signature)
                .map_err(|err| io::Error::other(format!("подпись: {err:?}")))?,
        };
        self.call(Request_::Command::OtaStart(start), slow)
            .map(outcome)
    }

    /// Весь образ: `ota_start`, куски по `OTA_CHUNK_BYTES`, `ota_finish`.
    /// Первый отказ обрывает загрузку и возвращается как есть.
    ///
    /// # Errors
    ///
    /// Канал порвался или ответа нет в срок.
    pub fn upload(
        &mut self,
        image: &[u8],
        signature: &[u8],
        slow: Duration,
    ) -> io::Result<Outcome> {
        if let Err(refused) = self.start(image.len(), signature, slow)? {
            return Ok(Err(refused));
        }
        for (index, data) in image.chunks(proto::OTA_CHUNK_BYTES).enumerate() {
            let chunk = proto::OtaChunk {
                data: heapless::Vec::from_slice(data)
                    .map_err(|err| io::Error::other(format!("кусок: {err:?}")))?,
                offset: u32::try_from(index * proto::OTA_CHUNK_BYTES).map_err(io::Error::other)?,
            };
            if let Err(refused) =
                outcome(self.call(Request_::Command::OtaChunk(chunk), REPLY_TIMEOUT)?)
            {
                return Ok(Err(refused));
            }
        }
        // `Finish` пишет запись для bootloader'а и стирает `STATE` — до секунды.
        self.call(
            Request_::Command::OtaFinish(proto::OtaFinish::default()),
            slow,
        )
        .map(outcome)
    }
    // {%- endif %}

    /// Запрос с новым `request_id` и ответ с тем же; чужие ответы (от
    /// прошлого запроса, на который не дождались) пропускаются. Канал,
    /// отказавший посреди, закрывается — следующий вызов откроет новый.
    fn call(
        &mut self,
        command: Request_::Command,
        timeout: Duration,
    ) -> io::Result<Response_::Result> {
        let id = self.next_id;
        self.next_id = self.next_id.wrapping_add(1);
        let mut encoder = PbEncoder::new(Vec::new());
        // В `Vec` запись не отказывает: ошибка — `Infallible`.
        let Ok(()) = Request {
            request_id: id,
            command: Some(command),
        }
        .encode(&mut encoder);
        let message = encoder.into_writer();

        let channel = match &mut self.channel {
            Some(channel) => channel,
            empty => empty.insert(open(&self.target)?),
        };
        let deadline = Instant::now() + timeout;
        let answer = channel.send(&message).and_then(|()| {
            loop {
                let mut response = Response::default();
                if response.decode_from_bytes(&channel.recv(deadline)?).is_ok()
                    && response.request_id == id
                {
                    break response
                        .result
                        .ok_or_else(|| io::Error::other("ответ без результата"));
                }
            }
        });
        if answer.is_err() {
            self.channel = None;
        }
        answer
    }
}

impl std::fmt::Debug for Device {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Device")
            .field("target", &self.target)
            .finish_non_exhaustive()
    }
}

/// Срок для медленных шагов OTA — стирания раздела на `ota_start`, `Finish`
/// и сброса с обменом разделов, — по размеру `ACTIVE` (`HOST_TARGET_ACTIVE_LEN`
/// от `xtask`).
///
/// Обмен в `embassy-boot` стирает и переписывает каждую страницу `ACTIVE` и
/// `DFU` по разу — вдвое больше байт, чем в `ACTIVE`; худший известный случай —
/// F0/F1 с записью полусловами, около 10 с на 128 KiB. Ждём не фиксированное
/// время, а ответа — предел лишь ограничивает ожидание сверху.
///
/// # Panics
///
/// `HOST_TARGET_ACTIVE_LEN` не задана или не число: сценарии OTA запускает
/// `cargo xtask test host-target`.
#[must_use]
pub fn slow_timeout() -> Duration {
    const BASE: Duration = Duration::from_secs(20);
    const PER_128K: Duration = Duration::from_secs(10);
    let active: u32 = env::var("HOST_TARGET_ACTIVE_LEN")
        .ok()
        .and_then(|len| len.parse().ok())
        .expect("HOST_TARGET_ACTIVE_LEN — сценарии OTA запускает `cargo xtask test host-target`");
    BASE + PER_128K * (2 * active).div_ceil(128 * 1024)
}

// {%- if ota == "true" %}
/// `Ok` или текст `Error` ответа.
fn outcome(result: Response_::Result) -> Outcome {
    match result {
        Response_::Result::Ok(_) => Ok(()),
        Response_::Result::Error(error) => Err(error.text.as_str().to_owned()),
        info @ Response_::Result::Info(_) => Err(format!("неожиданный ответ {info:?}")),
    }
}
// {%- endif %}

/// Адрес из окружения; `Ok(None)` — не задан.
fn target_from_env() -> Result<Option<Target>, String> {
    if let Some(raw) = var("HOST_TARGET_ADDR") {
        let (kind, addr) = match raw.split_once("://") {
            Some((kind, addr)) => (kind, addr),
            None => (NET_KIND, raw.as_str()),
        };
        let addr = net_addr(addr)?;
        return match kind {
            // {%- if cobs_used == "true" %}
            "tcp" => Ok(Some(Target::Tcp(addr))),
            // {%- endif %}
            "udp" => Ok(Some(Target::Udp(addr))),
            "" => Err(format!(
                "HOST_TARGET_ADDR={raw}: сетевого транспорта с protobuf в проекте нет — задайте \
                 HOST_TARGET_PORT"
            )),
            other => Err(format!(
                "HOST_TARGET_ADDR={raw}: транспорт {other} не поддержан"
            )),
        };
    }
    if let Some(port) = var("HOST_TARGET_PORT") {
        // {%- if protobuf_serial == "true" %}
        let baud = match var("HOST_TARGET_BAUD") {
            Some(baud) => baud
                .parse()
                .map_err(|err| format!("HOST_TARGET_BAUD={baud}: {err}"))?,
            None => DEFAULT_BAUD,
        };
        return Ok(Some(Target::Serial { port, baud }));
        // {%- else %}
        //{{newline}}        return Err(format!("HOST_TARGET_PORT={port}: protobuf по UART или USB в проекте нет — задайте HOST_TARGET_ADDR"));
        // {%- endif %}
    }
    Ok(None)
}

/// Переменная окружения; пустая — как не заданная: так её отдаёт CI, когда
/// переменной репозитория нет.
fn var(name: &str) -> Option<String> {
    env::var(name).ok().filter(|value| !value.is_empty())
}

/// `ip` или `ip:порт` (и имя хоста) — в адрес; порт по умолчанию — [`NET_PORT`].
fn net_addr(raw: &str) -> Result<SocketAddr, String> {
    // Голый IPv6 (`fe80::1`) тоже с двоеточиями — его порт добавляем сами.
    if let Ok(ip) = raw.parse::<std::net::IpAddr>() {
        return Ok(SocketAddr::new(ip, NET_PORT));
    }
    let with_port = if raw.contains(':') {
        raw.to_owned()
    } else {
        format!("{raw}:{NET_PORT}")
    };
    with_port
        .to_socket_addrs()
        .map_err(|err| format!("HOST_TARGET_ADDR={raw}: {err}"))?
        .next()
        .ok_or_else(|| format!("HOST_TARGET_ADDR={raw}: адрес не найден"))
}

/// Открывает канал до `target`.
fn open(target: &Target) -> io::Result<Box<dyn Channel>> {
    Ok(match target {
        // {%- if cobs_used == "true" %}
        Target::Tcp(addr) => {
            let stream = TcpStream::connect_timeout(addr, REPLY_TIMEOUT)?;
            stream.set_nodelay(true)?;
            Box::new(Framed::new(stream))
        }
        // {%- endif %}
        Target::Udp(addr) => {
            let socket = UdpSocket::bind(if addr.is_ipv4() {
                "0.0.0.0:0"
            } else {
                "[::]:0"
            })?;
            socket.connect(addr)?;
            Box::new(Datagram(socket))
        }
        // {%- if protobuf_serial == "true" %}
        Target::Serial { port, baud } => {
            let wire = serialport::new(port, *baud)
                .timeout(REPLY_TIMEOUT)
                .open()
                .map_err(io::Error::from)?;
            Box::new(Framed::new(wire))
        } // {%- endif %}
    })
}
// {%- endif %}

#[cfg(test)]
mod tests {
    // {%- if cobs_used == "true" %}
    use std::time::{Duration, Instant};

    use super::{Channel, Framed, Wire};

    /// Поток в памяти: что записано и что прочтётся.
    struct Loop {
        written: Vec<u8>,
        incoming: std::io::Cursor<Vec<u8>>,
    }

    impl std::io::Read for Loop {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            match self.incoming.read(buf)? {
                0 => Err(std::io::ErrorKind::TimedOut.into()),
                n => Ok(n),
            }
        }
    }

    impl std::io::Write for Loop {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.written.extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl Wire for Loop {
        fn set_timeout(&mut self, _: Duration) -> std::io::Result<()> {
            Ok(())
        }
    }

    /// Кадр — как в `adapters::cobs`: `0x00`, COBS от сообщения с CRC
    /// младшим байтом первым, `0x00`. Два кадра подряд с мусором между ними
    /// разбираются по одному, битый пропускается.
    #[test]
    fn frames_round_trip_and_survive_garbage() {
        let mut tx = Framed::new(Loop {
            written: Vec::new(),
            incoming: std::io::Cursor::new(Vec::new()),
        });
        tx.send(&[1, 0, 2]).expect("запись в память");
        tx.send(&[3]).expect("запись в память");
        let mut stream = tx.wire.written.clone();
        assert_eq!(stream.first(), Some(&0));
        // Битый кадр (CRC не сходится) между двумя целыми.
        stream.splice(0..0, [0, 5, 9, 9, 9, 9, 0]);

        let mut rx = Framed::new(Loop {
            written: Vec::new(),
            incoming: std::io::Cursor::new(stream),
        });
        let deadline = Instant::now() + Duration::from_secs(1);
        assert_eq!(rx.recv(deadline).expect("первый кадр"), [1, 0, 2]);
        assert_eq!(rx.recv(deadline).expect("второй кадр"), [3]);
    }

    /// `info` по TCP против «устройства» на этом же хосте: запрос уходит
    /// кадром, ответ с тем же `request_id` разбирается, чужой — пропускается.
    #[test]
    fn info_over_tcp_matches_the_request_id() {
        use micropb::{MessageDecode, MessageEncode, PbEncoder};
        use proto::{Request, Response, Response_, heapless};

        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("порт");
        let addr = listener.local_addr().expect("адрес");
        let device = std::thread::spawn(move || {
            let (stream, _) = listener.accept().expect("подключение");
            let mut link = Framed::new(stream);
            let raw = link
                .recv(Instant::now() + Duration::from_secs(5))
                .expect("запрос");
            let mut request = Request::default();
            request.decode_from_bytes(&raw).expect("разбор запроса");
            for id in [request.request_id + 100, request.request_id] {
                let info = proto::Info {
                    version: heapless::String::try_from("1.2.3").expect("строка"),
                    ..proto::Info::default()
                };
                let mut encoder = PbEncoder::new(Vec::new());
                let Ok(()) = Response {
                    request_id: id,
                    result: Some(Response_::Result::Info(info)),
                }
                .encode(&mut encoder);
                link.send(&encoder.into_writer()).expect("ответ");
            }
        });

        let mut client = super::Device {
            target: super::Target::Tcp(addr),
            channel: None,
            next_id: 1,
        };
        assert_eq!(client.info().expect("Info").version, "1.2.3");
        device.join().expect("поток устройства");
    }
    // {%- endif %}

    // {%- if has_protobuf == "true" %}
    /// Адрес без порта получает порт узла протокола.
    #[test]
    fn an_address_without_port_gets_the_protocol_port() {
        let addr = super::net_addr("127.0.0.1").expect("адрес");
        assert_eq!(addr.port(), super::NET_PORT);
        let addr = super::net_addr("127.0.0.1:7").expect("адрес");
        assert_eq!(addr.port(), 7);
        let addr = super::net_addr("::1").expect("адрес");
        assert_eq!(addr.port(), super::NET_PORT);
    }
    // {%- endif %}
}
