//! Узел `MQTT` графа задач: клиент брокера (MQTT 5, `minimq`) поверх сокета
//! стека `embassy-net`.
//!
//! Плата сама подключается к брокеру и держит соединение; все темы лежат под
//! `<prefix>/<uid>/`, где `uid` — заводской номер чипа в hex, а `prefix`
//! задаёт сборка.
//!
//! | Тема | Направление | Содержимое |
//! |---|---|---|
//! | `status` | плата → брокер | `online`/`offline` (retain; `offline` — will) |
//! | `info` | плата → брокер | сведения об устройстве, JSON (retain) |
//! | `telemetry` | плата → брокер | время работы раз в [`TELEMETRY_INTERVAL`](crate::mqtt::TELEMETRY_INTERVAL), JSON |
//! | `cmd/reboot` | брокер → плата | перезагрузка |
//! | `cmd/reboot/result` | плата → брокер | исход команды |
//!
//! Здесь — имена тем, разбор команд, тексты сообщений, адрес брокера и одна
//! сессия с брокером над готовым потоком байтов (`serve`).

use defmt_or_log::{info, warn};
use embassy_time::{Duration, Instant, with_deadline};
use heapless::String;
use minimq::{
    Buffers, ConfigBuilder, ConfigError, Connection, Publication, QoS, RetainHandling, Session,
    SubscriptionOptions, TopicFilter, Will,
};
// {%- if ota == "true" %}
use ports::{OtaCommand, Progress};
// {%- endif %}

use crate::device::{DeviceInfo, UID_BYTES};
// {%- if ota == "true" %}
use crate::ota::OtaClient;
// {%- endif %}

/// Буфер входящих пакетов `minimq`: самая длинная команда с темой.
pub const RX_BUFFER: usize = 512;
/// Буфер исходящих пакетов `minimq`: `CONNECT` с Will и неподтверждённые
/// публикации `QoS` 1 (подписка, `status`, `info`).
pub const TX_BUFFER: usize = 1024;

/// Самый длинный префикс тем: вместе с `-` и номером чипа (24 hex) он даёт
/// 64 байта — предел идентификатора клиента.
pub const PREFIX_MAX: usize = 39;

/// Как часто плата публикует `telemetry`.
pub const TELEMETRY_INTERVAL: Duration = Duration::from_secs(10);

/// Hex-знаков в номере чипа.
const UID_HEX: usize = 2 * UID_BYTES;
/// Идентификатор клиента: `<prefix>-<uid>`.
const CLIENT_ID_LEN: usize = PREFIX_MAX + 1 + UID_HEX;
/// `<prefix>/<uid>` и самая длинная тема под ним (`/cmd/reboot/result`).
const TOPIC_LEN: usize = CLIENT_ID_LEN + "/cmd/reboot/result".len();

const REBOOT: &str = "/cmd/reboot";

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

/// Имена тем и идентификатор клиента одной платы.
#[derive(Debug, Clone)]
pub struct Topics {
    prefix: String<PREFIX_MAX>,
    uid: String<UID_HEX>,
}

impl Topics {
    /// Темы под `<prefix>/<uid>`.
    ///
    /// # Panics
    ///
    /// `prefix` длиннее [`PREFIX_MAX`]: сборка это уже проверила.
    #[must_use]
    pub fn new(prefix: &str, uid: &[u8; UID_BYTES]) -> Self {
        use core::fmt::Write;
        let mut hex = String::new();
        for b in uid {
            write!(hex, "{b:02x}").expect("номер чипа в hex влезает");
        }
        Self {
            prefix: String::try_from(prefix).expect("префикс не длиннее PREFIX_MAX"),
            uid: hex,
        }
    }

    fn topic<const N: usize>(&self, suffix: &str) -> String<N> {
        use core::fmt::Write;
        let mut s = String::new();
        write!(s, "{}/{}{suffix}", self.prefix, self.uid).expect("тема влезает в N");
        s
    }

    /// Тема состояния.
    #[must_use]
    pub fn status(&self) -> String<{ CLIENT_ID_LEN + "/status".len() }> {
        self.topic("/status")
    }

    /// Тема сведений об устройстве.
    #[must_use]
    pub fn info(&self) -> String<{ CLIENT_ID_LEN + "/info".len() }> {
        self.topic("/info")
    }

    /// Тема телеметрии.
    #[must_use]
    pub fn telemetry(&self) -> String<{ CLIENT_ID_LEN + "/telemetry".len() }> {
        self.topic("/telemetry")
    }

    /// Фильтр подписки на все команды.
    #[must_use]
    pub fn cmd_filter(&self) -> String<{ CLIENT_ID_LEN + "/cmd/#".len() }> {
        self.topic("/cmd/#")
    }

    /// Тема исхода `reboot`.
    #[must_use]
    pub fn reboot_result(&self) -> String<TOPIC_LEN> {
        self.topic("/cmd/reboot/result")
    }

    /// Идентификатор клиента: `<prefix>-<uid>`.
    #[must_use]
    pub fn client_id(&self) -> String<CLIENT_ID_LEN> {
        use core::fmt::Write;
        let mut s = String::new();
        write!(s, "{}-{}", self.prefix, self.uid).expect("идентификатор влезает");
        s
    }
}

/// Команда с брокера.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Перезагрузить МК.
    Reboot,
}

/// Команда по теме сообщения; `None` — чужая или неизвестная тема.
#[must_use]
pub fn parse_command(topics: &Topics, topic: &str) -> Option<Command> {
    let rest = topic
        .strip_prefix(topics.prefix.as_str())?
        .strip_prefix('/')?
        .strip_prefix(topics.uid.as_str())?;
    match rest {
        REBOOT => Some(Command::Reboot),
        _ => None,
    }
}

/// Тело `telemetry`: JSON-объект со временем работы.
///
/// # Errors
///
/// Ошибка записи в `out`: объект не влез в буфер.
pub fn write_telemetry(out: &mut impl core::fmt::Write, uptime_s: u64) -> core::fmt::Result {
    // Скобки — `write_char`: это исходник шаблона, см. `device::write_info`.
    out.write_char('{')?;
    write!(out, r#""uptime_s":{uptime_s}"#)?;
    out.write_char('}')
}

/// Чем кончилась сессия — для строки лога узла.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(not(test), expect(dead_code, reason = "сессию зовёт узел MQTT"))]
pub(crate) enum Ended {
    /// Брокер не принял подключение.
    Refused,
    /// Плата была в сети, и связь пропала.
    Lost,
}

/// Что нужно сессии, кроме потока и адреса брокера.
#[cfg_attr(not(test), expect(dead_code, reason = "сессию зовёт узел MQTT"))]
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
    pub ota: &'a OtaClient,
    // {%- endif %}
}

/// Настройки клиента: идентификатор, вход (при непустом `user`) и Will
/// `offline` на `status`.
fn config<'a>(
    endpoint: &Endpoint,
    topics: &Topics,
    rx: &'a mut [u8],
    tx: &'a mut [u8],
) -> Result<ConfigBuilder<'a>, ConfigError> {
    let mut config = ConfigBuilder::new(Buffers::new(rx, tx)).client_id(&topics.client_id())?;
    if !endpoint.user.is_empty() {
        config = config.auth(endpoint.user, endpoint.password.as_bytes())?;
    }
    config.will(
        Will::new(&topics.status(), b"offline", &[])?
            .retained()
            .qos(QoS::AtLeastOnce),
    )
}

/// Начало сессии: подписка на команды, `status` = `online` и `info`.
async fn announce<IO: minimq::Io>(
    conn: &mut Connection<'_, '_, IO>,
    topics: &Topics,
    info: &DeviceInfo,
    mac: [u8; 6],
    ip: Option<embassy_net::Ipv4Address>,
) -> Result<(), ()> {
    let filter = topics.cmd_filter();
    let options = SubscriptionOptions::default()
        // Команда, оставленная с retain, иначе приходила бы при каждом
        // подключении.
        .retain_behavior(RetainHandling::Never)
        .maximum_qos(QoS::AtLeastOnce);
    conn.subscribe(&[TopicFilter::new(&filter).options(options)], &[])
        .await
        .map_err(drop)?;
    // `online` — каждый раз: брокер мог уже разослать Will `offline`.
    let status = topics.status();
    conn.publish(
        Publication::text(&status, "online")
            .qos(QoS::AtLeastOnce)
            .retain(),
    )
    .await
    .map_err(drop)?;
    let mut json = String::<256>::new();
    if crate::device::write_info(&mut json, info, mac, ip, Instant::now().as_secs()).is_err() {
        warn!("mqtt: сведения об устройстве не влезли в буфер");
        return Ok(());
    }
    let topic = topics.info();
    conn.publish(
        Publication::text(&topic, &json)
            .qos(QoS::AtLeastOnce)
            .retain(),
    )
    .await
    .map_err(drop)?;
    Ok(())
}

/// Одна сессия с брокером над установленным соединением `io`: подключение с
/// Will, подписка на команды, `status` и `info`, дальше телеметрия и команды,
/// пока связь жива. Паузы между сессиями — не здесь: их держит транспорт.
#[cfg_attr(not(test), expect(dead_code, reason = "сессию зовёт узел MQTT"))]
pub(crate) async fn serve<IO: minimq::Io>(io: IO, endpoint: &Endpoint, env: &mut Env<'_>) -> Ended {
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
    let Ok(config) = config(endpoint, topics, rx, tx) else {
        warn!("mqtt: настройки клиента не приняты");
        return Ended::Refused;
    };
    let mut session = Session::new(config);
    let Ok(mut conn) = session.connect(io).await else {
        warn!(
            "mqtt: брокер {}.{}.{}.{}:{} не принял подключение — проверьте mqtt.toml",
            a, b, c, d, port
        );
        return Ended::Refused;
    };
    info!("mqtt: подключён к брокеру {}.{}.{}.{}:{}", a, b, c, d, port);

    // Любая ошибка `minimq` после подключения — связь потеряна: соединение
    // больше не годится, следующая сессия — с новым.
    let online = async {
        announce(&mut conn, topics, info, *mac, *ip).await?;
        let mut next = Instant::now() + TELEMETRY_INTERVAL;
        loop {
            let command = match with_deadline(next, conn.recv()).await {
                // `recv` отменяется без потерь; публикация `QoS` 0 — нет, она
                // вне таймаута.
                Err(_) => {
                    let mut json = String::<40>::new();
                    write_telemetry(&mut json, Instant::now().as_secs())
                        .expect("u64 в объекте влезает в 40 байт");
                    let topic = topics.telemetry();
                    conn.publish(Publication::text(&topic, &json))
                        .await
                        .map_err(drop)?;
                    next = fsm::next_tick(next, TELEMETRY_INTERVAL, Instant::now());
                    continue;
                }
                Ok(Err(_)) => return Err(()),
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
                    let reply = ota.request(OtaCommand::Reboot).await;
                    let text = if matches!(reply, Ok(Ok(Progress::Rebooting))) {
                        "accepted"
                    } else {
                        "refused"
                    };
                    let topic = topics.reboot_result();
                    conn.publish(Publication::text(&topic, text).qos(QoS::AtLeastOnce))
                        .await
                        .map_err(drop)?;
                    // {%- endif %}
                    // {%- if ota != "true" %}
                    warn!("mqtt: перезагрузка без OTA недоступна");
                    // {%- endif %}
                }
            }
        }
    };
    let Err(()): Result<core::convert::Infallible, ()> = online.await;
    warn!(
        "mqtt: связь с брокером {}.{}.{}.{}:{} пропала",
        a, b, c, d, port
    );
    Ended::Lost
}

#[cfg(test)]
pub(crate) mod tests {
    use core::pin::Pin;
    use core::task::{Context, Poll, Waker};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::vec::Vec;

    use embassy_time::MockDriver;
    use embedded_io_async::{ErrorKind, ErrorType, Read, Write};

    use super::*;

    #[test]
    fn topics_are_prefix_uid_suffix() {
        let t = Topics::new("devices", &[0xab; 12]);
        let uid = "abababababababababababab";
        assert_eq!(t.status().as_str(), format!("devices/{uid}/status"));
        assert_eq!(t.info().as_str(), format!("devices/{uid}/info"));
        assert_eq!(t.telemetry().as_str(), format!("devices/{uid}/telemetry"));
        assert_eq!(t.cmd_filter().as_str(), format!("devices/{uid}/cmd/#"));
        assert_eq!(
            t.reboot_result().as_str(),
            format!("devices/{uid}/cmd/reboot/result")
        );
        assert_eq!(t.client_id().as_str(), format!("devices-{uid}"));
    }

    #[test]
    fn longest_prefix_fits_client_id() {
        let t = Topics::new(&"p".repeat(PREFIX_MAX), &[0; 12]);
        assert_eq!(t.client_id().len(), 64);
    }

    #[test]
    fn commands_are_parsed_by_topic() {
        let t = Topics::new("d", &[1; 12]);
        let base = "d/010101010101010101010101";
        assert!(matches!(
            parse_command(&t, &format!("{base}/cmd/reboot")),
            Some(Command::Reboot)
        ));
        assert!(parse_command(&t, &format!("{base}/cmd/unknown")).is_none());
        assert!(parse_command(&t, "other/cmd/reboot").is_none());
        assert!(parse_command(&t, &format!("{base}/cmd/reboot/result")).is_none());
    }

    #[test]
    fn telemetry_is_json() {
        let mut s = std::string::String::new();
        write_telemetry(&mut s, 42).unwrap();
        assert_eq!(s, r#"{"uptime_s":42}"#);
    }

    /// Брокер в памяти — поток байтов для [`serve`]: вход — то, что «прислал»
    /// брокер ([`FakeBroker::push`]), выход — всё, что записала плата.
    /// Клоны делят одно состояние: один уходит в `serve`, другим тест
    /// подкладывает пакеты и читает выход. Пустой вход — `read` в `Pending`
    /// без пробуждения (так `minimq` ждёт данных; `TimedOut` он счёл бы
    /// обрывом), поэтому future опрашивают вручную, как в [`poll`].
    #[derive(Clone, Default)]
    pub(crate) struct FakeBroker(Rc<RefCell<Wire>>);

    #[derive(Default)]
    struct Wire {
        input: VecDeque<u8>,
        output: Vec<u8>,
        fail_reads: bool,
    }

    impl FakeBroker {
        /// Пакет от брокера.
        pub(crate) fn push(&self, bytes: &[u8]) {
            self.0.borrow_mut().input.extend(bytes);
        }

        /// Всё, что плата записала с начала.
        pub(crate) fn output(&self) -> Vec<u8> {
            self.0.borrow().output.clone()
        }

        /// Дальше `read` возвращает ошибку: связь оборвалась.
        pub(crate) fn fail_reads(&self) {
            self.0.borrow_mut().fail_reads = true;
        }
    }

    impl ErrorType for FakeBroker {
        type Error = ErrorKind;
    }

    impl Read for FakeBroker {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ErrorKind> {
            core::future::poll_fn(|_| {
                let mut wire = self.0.borrow_mut();
                if wire.fail_reads {
                    return Poll::Ready(Err(ErrorKind::ConnectionReset));
                }
                if wire.input.is_empty() {
                    return Poll::Pending;
                }
                let n = buf.len().min(wire.input.len());
                for (dst, src) in buf.iter_mut().zip(wire.input.drain(..n)) {
                    *dst = src;
                }
                Poll::Ready(Ok(n))
            })
            .await
        }
    }

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "сигнатуру задаёт трейт; запись в память не ждёт"
    )]
    impl Write for FakeBroker {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, ErrorKind> {
            self.0.borrow_mut().output.extend_from_slice(buf);
            Ok(buf.len())
        }

        async fn flush(&mut self) -> Result<(), ErrorKind> {
            Ok(())
        }
    }

    /// CONNACK с кодом причины `code` (`0` — принято).
    pub(crate) fn connack(code: u8) -> [u8; 5] {
        [0x20, 0x03, 0x00, code, 0x00]
    }

    /// SUBACK на подписку `id` с выданным `QoS` 1.
    pub(crate) fn suback(id: u16) -> [u8; 6] {
        let [hi, lo] = id.to_be_bytes();
        [0x90, 0x04, hi, lo, 0x00, 0x01]
    }

    /// PUBACK на публикацию `id`.
    pub(crate) fn puback(id: u16) -> [u8; 4] {
        let [hi, lo] = id.to_be_bytes();
        [0x40, 0x02, hi, lo]
    }

    /// Входящий PUBLISH `QoS` 0 без свойств.
    pub(crate) fn publish(topic: &str, payload: &[u8], retain: bool) -> Vec<u8> {
        let remaining = 2 + topic.len() + 1 + payload.len();
        let mut packet = vec![0x30 | u8::from(retain), u8::try_from(remaining).unwrap()];
        packet.extend_from_slice(&u16::try_from(topic.len()).unwrap().to_be_bytes());
        packet.extend_from_slice(topic.as_bytes());
        packet.push(0x00);
        packet.extend_from_slice(payload);
        packet
    }

    /// Исходящая публикация, разобранная из байтов платы.
    #[derive(Debug)]
    pub(crate) struct Sent {
        pub(crate) retain: bool,
        pub(crate) topic: std::string::String,
        pub(crate) payload: Vec<u8>,
    }

    /// Все PUBLISH в `out`. Длина свойств — один байт: исходящим публикациям
    /// свойства не нужны.
    pub(crate) fn sent(out: &[u8]) -> Vec<Sent> {
        let mut found = Vec::new();
        let mut rest = out;
        while let [header, tail @ ..] = rest {
            // Оставшаяся длина — varint: по 7 бит, старший — «дальше ещё».
            let (mut len, mut shift, mut at) = (0, 0, 0);
            loop {
                len |= usize::from(tail[at] & 0x7F) << shift;
                at += 1;
                shift += 7;
                if tail[at - 1] & 0x80 == 0 {
                    break;
                }
            }
            let (body, next) = tail[at..].split_at(len);
            rest = next;
            if header & 0xF0 != 0x30 {
                continue;
            }
            let topic_len = usize::from(u16::from_be_bytes([body[0], body[1]]));
            let topic = std::str::from_utf8(&body[2..2 + topic_len]).unwrap().into();
            let qos = (header >> 1) & 0x03;
            let props = 2 + topic_len + if qos > 0 { 2 } else { 0 };
            found.push(Sent {
                retain: header & 0x01 != 0,
                topic,
                payload: body[props + 1 + usize::from(body[props])..].to_vec(),
            });
        }
        found
    }

    /// Один опрос future с пустым waker'ом.
    pub(crate) fn poll<F: Future + ?Sized>(future: Pin<&mut F>) -> Poll<F::Output> {
        future.poll(&mut Context::from_waker(Waker::noop()))
    }

    const ENDPOINT: Endpoint = Endpoint {
        addr: [10, 0, 0, 1],
        port: 1883,
        user: "",
        password: "",
    };

    const UID: [u8; UID_BYTES] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    const UID_TEXT: &str = "0102030405060708090a0b0c";

    /// Окружение сессии на всё время теста: буферы и темы — `'static`.
    fn env(
        // {%- if ota == "true" %}
        ota: crate::ota::OtaClient,
        // {%- endif %}
    ) -> &'static mut Env<'static> {
        Box::leak(Box::new(Env {
            topics: Box::leak(Box::new(Topics::new("devices", &UID))),
            info: Box::leak(Box::new(DeviceInfo {
                version: "1.2.3",
                commit: "abcdef12",
                chip: "stm32f407ve",
                uid: UID,
                flash_size: 512 * 1024,
                sys_hz: None,
            })),
            mac: [2, 0, 0, 0, 0, 1],
            ip: None,
            rx: Box::leak(Box::new([0; RX_BUFFER])),
            tx: Box::leak(Box::new([0; TX_BUFFER])),
            // {%- if ota == "true" %}
            ota: Box::leak(Box::new(ota)),
            // {%- endif %}
        }))
    }

    /// Окружение{% if ota == "true" %} с клиентом OTA, которого никто не обслуживает{% endif %}.
    fn idle_env() -> &'static mut Env<'static> {
        env(
            // {%- if ota == "true" %}
            crate::ota::tests::live().0,
            // {%- endif %}
        )
    }

    type Session = Pin<Box<dyn Future<Output = Ended>>>;

    /// Сессия, которую брокер принял: CONNACK и SUBACK уже во входе.
    fn online(env: &'static mut Env<'static>) -> (FakeBroker, Session) {
        let broker = FakeBroker::default();
        broker.push(&connack(0));
        broker.push(&suback(1));
        let mut session: Session = Box::pin(serve(broker.clone(), &ENDPOINT, env));
        assert!(poll(session.as_mut()).is_pending(), "сессия в сети");
        (broker, session)
    }

    fn topic(suffix: &str) -> std::string::String {
        format!("devices/{UID_TEXT}{suffix}")
    }

    fn contains(haystack: &[u8], needle: &[u8]) -> bool {
        haystack.windows(needle.len()).any(|w| w == needle)
    }

    #[test]
    fn connect_carries_will_and_client_id() {
        let broker = FakeBroker::default();
        let mut session = Box::pin(serve(broker.clone(), &ENDPOINT, idle_env()));
        assert!(poll(session.as_mut()).is_pending(), "ждёт CONNACK");
        let out = broker.output();
        assert_eq!(out[0], 0x10, "первым — CONNECT");
        assert!(contains(&out, format!("devices-{UID_TEXT}").as_bytes()));
        assert!(contains(&out, topic("/status").as_bytes()));
        assert!(contains(&out, b"offline"));
    }

    #[test]
    fn online_publishes_status_and_info_retained() {
        let (broker, _session) = online(idle_env());
        let sent = sent(&broker.output());
        let status = sent.iter().find(|p| p.topic == topic("/status")).unwrap();
        assert_eq!(status.payload, b"online");
        assert!(status.retain);
        let info = sent.iter().find(|p| p.topic == topic("/info")).unwrap();
        assert!(contains(&info.payload, br#""chip":"#));
        assert!(info.retain);
    }

    #[test]
    fn telemetry_after_interval() {
        let (broker, mut session) = online(idle_env());
        MockDriver::get().advance(TELEMETRY_INTERVAL);
        assert!(poll(session.as_mut()).is_pending());
        let sent = sent(&broker.output());
        let telemetry = sent
            .iter()
            .find(|p| p.topic == topic("/telemetry"))
            .expect("телеметрия опубликована");
        assert!(telemetry.payload.starts_with(br#"{"uptime_s":"#));
        assert!(!telemetry.retain);
    }

    #[test]
    fn refused_connect_reports_refused() {
        let broker = FakeBroker::default();
        broker.push(&connack(0x86));
        let mut session = Box::pin(serve(broker, &ENDPOINT, idle_env()));
        assert_eq!(poll(session.as_mut()), Poll::Ready(Ended::Refused));
    }

    #[test]
    fn transport_error_ends_session() {
        let (broker, mut session) = online(idle_env());
        broker.fail_reads();
        assert_eq!(poll(session.as_mut()), Poll::Ready(Ended::Lost));
    }

    #[test]
    fn unknown_command_keeps_session() {
        let (broker, mut session) = online(idle_env());
        broker.push(&puback(2));
        broker.push(&puback(3));
        broker.push(&publish(&topic("/cmd/foo"), b"", false));
        assert!(poll(session.as_mut()).is_pending());
        MockDriver::get().advance(TELEMETRY_INTERVAL);
        assert!(poll(session.as_mut()).is_pending());
        let sent = sent(&broker.output());
        assert!(sent.iter().any(|p| p.topic == topic("/telemetry")));
    }

    // {%- if ota == "true" %}
    /// Сессия и узел OTA по очереди, пока оба не встанут.
    fn spin(session: &mut Session, node: &mut crate::ota::tests::Node) {
        for _ in 0..4 {
            assert!(poll(session.as_mut()).is_pending(), "сессия жива");
            assert!(poll(node.as_mut()).is_pending(), "узел OTA не выходит");
        }
    }

    #[test]
    fn retained_reboot_is_ignored() {
        let (ota, mut node) = crate::ota::tests::live();
        let (broker, mut session) = online(env(ota));
        broker.push(&publish(&topic("/cmd/reboot"), b"", true));
        spin(&mut session, &mut node);
        // Узел, принявший `Reboot`, через `REBOOT_GRACE` сбросил бы МК —
        // фейк паникует; интервал телеметрии длиннее.
        MockDriver::get().advance(TELEMETRY_INTERVAL);
        spin(&mut session, &mut node);
        let sent = sent(&broker.output());
        assert!(!sent.iter().any(|p| p.topic == topic("/cmd/reboot/result")));
        assert!(sent.iter().any(|p| p.topic == topic("/telemetry")));
    }

    #[test]
    fn reboot_command_reaches_ota() {
        let (ota, mut node) = crate::ota::tests::live();
        let (broker, mut session) = online(env(ota));
        broker.push(&publish(&topic("/cmd/reboot"), b"", false));
        assert!(poll(session.as_mut()).is_pending(), "запрос узлу OTA");
        assert!(poll(node.as_mut()).is_pending(), "узел ответил");
        // Дальше узел не опрашивается: через `REBOOT_GRACE` он сбросил бы МК.
        assert!(poll(session.as_mut()).is_pending(), "исход опубликован");
        let sent = sent(&broker.output());
        let result = sent
            .iter()
            .find(|p| p.topic == topic("/cmd/reboot/result"))
            .expect("исход команды опубликован");
        assert_eq!(result.payload, b"accepted");
    }
    // {%- endif %}
}
