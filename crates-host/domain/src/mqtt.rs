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

use core::net::SocketAddrV4;

use defmt_or_log::{FormatOrDebug, info, warn};
use embassy_time::{Duration, Instant, with_deadline, with_timeout};
use embedded_io_async::{Read, Write};
use heapless::String;
use minimq::{
    Buffers, ConfigBuilder, ConfigError, Connection, Publication, QoS, RetainHandling, Session,
    SubscriptionOptions, TopicFilter, Will,
};
use ports::dial::Dial;
use supervisor::runtime::TaskExit;
// {%- if ota == "true" %}
use ports::{OtaCommand, Progress};
// {%- endif %}

use crate::device::{DeviceInfo, INFO_MAX, UID_BYTES};
// {%- if ota == "true" %}
use crate::ota::OtaClient;
// {%- endif %}

/// Буфер входящих пакетов `minimq`: самая длинная команда с темой.
pub const RX_BUFFER: usize = 512;
/// Буфер исходящих пакетов `minimq`: `CONNECT` с Will и входом (его место
/// `minimq` держит всю сессию) и неподтверждённые пакеты `QoS` 1 — подписка,
/// `status`, `info`, исход `reboot`. Сумму держит `const`-assert у `TX_BUDGET`.
pub const TX_BUFFER: usize = 1152;

/// Самые длинные имя и пароль на брокере вместе, байт: они едут в `CONNECT`,
/// а его место занято в [`TX_BUFFER`] всю сессию. Тот же предел держит разбор
/// `mqtt.toml`.
pub const AUTH_MAX: usize = 256;

/// Самый длинный префикс тем: вместе с `-` и номером чипа (24 hex) он даёт
/// 64 байта — предел идентификатора клиента.
pub const PREFIX_MAX: usize = 39;

/// Как часто плата публикует `telemetry`.
pub const TELEMETRY_INTERVAL: Duration = Duration::from_secs(10);

/// Сколько ждать CONNACK: брокер, который принял TCP, но молчит на `CONNECT`,
/// иначе держал бы узел вечно и без строки в логе.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Hex-знаков в номере чипа.
const UID_HEX: usize = 2 * UID_BYTES;
/// Идентификатор клиента: `<prefix>-<uid>`.
const CLIENT_ID_LEN: usize = PREFIX_MAX + 1 + UID_HEX;
/// `<prefix>/<uid>` и самая длинная тема под ним (`/cmd/reboot/result`).
const TOPIC_LEN: usize = CLIENT_ID_LEN + "/cmd/reboot/result".len();

const REBOOT: &str = "/cmd/reboot";

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

/// Сколько `minimq` держит в [`TX_BUFFER`] в худший момент сессии (кодирование —
/// `minimq` 0.13: `handshake.rs`, `outbound.rs`). `CONNECT`: запас под заголовок
/// (5), имя протокола, версия, флаги, keep-alive, три свойства (13 байт и
/// длина), идентификатор, Will `offline` на `status` и вход. Дальше — всё, что
/// может разом ждать подтверждения: подписка, `status`, `info` и исход `reboot`;
/// последний `minimq` кодирует с запасом в 5 байт под заголовок, а не с его
/// длиной.
const TX_BUDGET: usize = 5
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
    + packet_len(publish_rest(CLIENT_ID_LEN + "/info".len(), INFO_MAX))
    + 5
    + publish_rest(TOPIC_LEN, "accepted".len());
const _: () = assert!(
    TX_BUDGET <= TX_BUFFER,
    "CONNECT и неподтверждённые пакеты не влезают в TX_BUFFER"
);

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
    pub ota: &'a OtaClient,
    // {%- endif %}
}

/// Настройки клиента: идентификатор, вход (при непустом `user`) и Will
/// `offline` на `status`.
fn config<'a>(
    endpoint: &Endpoint,
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
) -> Result<(), ()> {
    let filter = topics.cmd_filter();
    let options = SubscriptionOptions::default()
        // Команда, оставленная с retain, иначе приходила бы при каждом
        // подключении.
        .retain_behavior(RetainHandling::Never)
        // Свой `cmd/reboot/result` не возвращается «неизвестной командой».
        .ignore_local_messages()
        .maximum_qos(QoS::AtLeastOnce);
    conn.subscribe(&[TopicFilter::new(&filter).options(options)], &[])
        .await
        .map_err(drop)?;
    // `online` — каждый раз: брокер мог уже разослать Will `offline`.
    conn.publish(
        Publication::text(status, "online")
            .qos(QoS::AtLeastOnce)
            .retain(),
    )
    .await
    .map_err(drop)?;
    let mut json = String::<INFO_MAX>::new();
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
/// пока связь жива. Паузы между попытками — не здесь: их держит транспорт.
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
        Ok(Err(_)) => {
            warn!(
                "mqtt: брокер {}.{}.{}.{}:{} не принял подключение — проверьте mqtt.toml",
                a, b, c, d, port
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

    // Любая ошибка `minimq` после подключения — связь потеряна: соединение
    // больше не годится, следующая сессия — с новым.
    let online = async {
        announce(&mut conn, topics, &status, info, *mac, *ip).await?;
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

/// Узел `MQTT`: подключается к брокеру, ведёт сессию, после её конца
/// подключается снова — пока жива прошивка; не возвращается.
///
/// У автомата нет выходного состояния, поэтому [`TaskExit`] недостижим: узел
/// не падает (`restart: Never`).
pub async fn task<B: Broker, D: Dial + Read + Write>(mut ctx: MqttCtx<'_, B, D>) -> TaskExit
where
    <D as Dial>::Error: FormatOrDebug,
{
    match ctx.run().await {}
}

#[fsm::typestate]
impl<B: Broker, D: Dial + Read + Write> MqttCtx<'_, B, D>
where
    <D as Dial>::Error: FormatOrDebug,
{
    fsm::machine! {
        machine Mqtt mod edges {
            states {
                /// Без сессии: ждёт соединения с брокером.
                Offline { topics: Topics },
            }
            start => begin -> Offline;
            listen {
                Offline => linked: self.dial(),
            }
            transitions {
                Offline + linked(Ok(endpoint)) => session -> self,
                Offline + linked(Err(ref failure)) => unreachable -> self,
            }
        }
    }

    /// Темы платы; первое подключение — после того, как стек получил адрес
    /// (статический или от DHCP).
    async fn begin(&mut self) -> Offline {
        info!("domain: узел MQTT запущен");
        self.stack.wait_config_up().await;
        Offline {
            topics: Topics::new(self.prefix, &self.info.uid),
        }
    }

    /// Адрес брокера и одна попытка подключиться к нему; паузу перед ней
    /// выдерживает `Dial`.
    async fn dial(&mut self) -> Result<Endpoint, (Endpoint, <D as Dial>::Error)> {
        let endpoint = self.broker.endpoint().await;
        match self
            .link
            .dial(SocketAddrV4::new(endpoint.addr.into(), endpoint.port))
            .await
        {
            Ok(()) => Ok(endpoint),
            Err(err) => Err((endpoint, err)),
        }
    }

    /// Брокер недоступен — строка в лог на каждую попытку, и узел сразу
    /// зовёт `dial` снова: адрес-заглушка из `mqtt.toml` иначе молчал бы.
    #[expect(
        clippy::unused_self,
        reason = "сигнатуру действия задаёт таблица `#[fsm::typestate]`"
    )]
    fn unreachable(&mut self, failure: &(Endpoint, <D as Dial>::Error)) {
        let (endpoint, err) = failure;
        let [a, b, c, d] = endpoint.addr;
        warn!(
            "mqtt: брокер {}.{}.{}.{}:{} недоступен ({:?}) — проверьте mqtt.toml",
            a, b, c, d, endpoint.port, err
        );
    }

    /// Одна сессия над открытым соединением. Чем бы она ни кончилась (строку
    /// лога пишет `serve`), трубка кладётся, и узел сразу подключается снова.
    async fn session(&mut self, s: &mut Offline, endpoint: Endpoint) {
        // Среда в стеке одна — Ethernet (`medium-ethernet`).
        let embassy_net::HardwareAddress::Ethernet(mac) = self.stack.hardware_address();
        let mut env = Env {
            topics: &s.topics,
            info: self.info,
            mac: mac.0,
            ip: self
                .stack
                .config_v4()
                .map(|config| config.address.address()),
            rx: &mut self.mqtt_rx[..],
            tx: &mut self.mqtt_tx[..],
            // {%- if ota == "true" %}
            ota: self.ota,
            // {%- endif %}
        };
        serve(&mut *self.link, &endpoint, &mut env).await;
        self.link.hang_up();
    }
}

/// Вход узла `MQTT`: стек, сведения, префикс тем, откуда адрес брокера и
/// память соединения `D` с ним.
///
/// Соединение собирает узел (`D::assemble(stack, link)` — порт
/// [`Assemble`](ports::Assemble)) на том же стеке; compose-site раздаёт только
/// стек платы и ссылки на свои статики. Написан руками, а не выведен
/// фрагментом: слот соединения инициализируется сборкой, а не чтением поля.
pub struct MqttWiring<B, D: ports::Assemble<Source = crate::net::Stack<'static>>> {
    /// Сетевой стек — ради адреса платы и MAC в `info` и под соединение.
    pub stack: crate::net::Stack<'static>,
    /// Сведения для `info`.
    pub info: crate::device::DeviceInfo,
    /// Префикс тем, не длиннее `PREFIX_MAX`.
    pub prefix: &'static str,
    /// Откуда адрес брокера.
    pub broker: B,
    /// Память соединения с брокером.
    pub link: D::Buffers,
    // {%- if ota == "true" %}
    /// Клиент входа `CMD` узла OTA — для `cmd/reboot`.
    pub ota: crate::ota::OtaClient,
    // {%- endif %}
}

// Вручную: у стека `embassy-net` нет `Debug`.
impl<B, D: ports::Assemble<Source = crate::net::Stack<'static>>> core::fmt::Debug
    for MqttWiring<B, D>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MqttWiring")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}

// Узел объявлен рядом со своей задачей — как `SSH`. `<B>` — откуда адрес
// брокера (`Fixed` или свой тип проекта), `<D>` — соединение (`Dial` +
// поток байтов; `adapters::net::TcpClient` над сокетом стека — собирает его
// слот `LINK` из стека и буферов TCP от `app`); `local` у них и у стека — все
// вправе быть `!Send`.
// Буферы `minimq` — слоты `= static`: через стек они не идут. Без
// `watchdog:`: брокера может не быть всю жизнь платы.
supervisor::supervisor_fragment! {
    name: MQTT_FRAG<B, D>;
    boot: inputs: $crate::mqtt::MqttWiring<B, D>;

    node deps: [], restart: Never,
        resources: [
            /// Сетевой стек — ради адреса платы и MAC в `info`.
            STACK: local $crate::net::Stack<'static> = inputs.stack,
            /// Сведения для `info`.
            INFO: $crate::device::DeviceInfo = inputs.info,
            /// Префикс тем, не длиннее `PREFIX_MAX`.
            PREFIX: &'static str = inputs.prefix,
            /// Откуда адрес брокера.
            BROKER: local B = inputs.broker,
            /// Соединение с брокером — собирает его узел на стеке платы.
            LINK: local D = <D as $crate::Assemble>::assemble(inputs.stack, inputs.link),
            // {%- if ota == "true" %}
            /// Клиент входа `CMD` узла OTA — для `cmd/reboot`.
            OTA: $crate::ota::OtaClient = inputs.ota,
            // {%- endif %}
            /// Входящие пакеты `minimq`.
            MQTT_RX: &'static mut [u8; $crate::mqtt::RX_BUFFER] = static,
            /// Исходящие пакеты `minimq`.
            MQTT_TX: &'static mut [u8; $crate::mqtt::TX_BUFFER] = static
        ],
        ctx: $crate::mqtt::MqttCtx,
        task: $crate::mqtt::task;
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

    /// Окружение сессии на всё время теста: буферы и темы — `'static`{% if ota == "true" %};
    /// клиент OTA — того узла, которого никто не обслуживает{% endif %}.
    fn env() -> &'static mut Env<'static> {
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
            ota: Box::leak(Box::new(crate::ota::tests::live().0)),
            // {%- endif %}
        }))
    }

    type Running = Pin<Box<dyn Future<Output = Ended>>>;

    /// Сессия, которую брокер принял: CONNACK и SUBACK уже во входе.
    fn online(env: &'static mut Env<'static>) -> (FakeBroker, Running) {
        let broker = FakeBroker::default();
        broker.push(&connack(0));
        broker.push(&suback(1));
        let mut session: Running = Box::pin(serve(broker.clone(), &ENDPOINT, env));
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
        let mut session = Box::pin(serve(broker.clone(), &ENDPOINT, env()));
        assert!(poll(session.as_mut()).is_pending(), "ждёт CONNACK");
        let out = broker.output();
        assert_eq!(out[0], 0x10, "первым — CONNECT");
        assert!(contains(&out, format!("devices-{UID_TEXT}").as_bytes()));
        assert!(contains(&out, topic("/status").as_bytes()));
        assert!(contains(&out, b"offline"));
    }

    #[test]
    fn online_publishes_status_and_info_retained() {
        let (broker, _session) = online(env());
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
        let (broker, mut session) = online(env());
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
        let mut session = Box::pin(serve(broker, &ENDPOINT, env()));
        assert_eq!(poll(session.as_mut()), Poll::Ready(Ended::Refused));
    }

    #[test]
    fn silent_broker_times_out_connect() {
        let broker = FakeBroker::default();
        let mut session = Box::pin(serve(broker, &ENDPOINT, env()));
        assert!(poll(session.as_mut()).is_pending(), "ждёт CONNACK");
        MockDriver::get().advance(CONNECT_TIMEOUT);
        assert_eq!(poll(session.as_mut()), Poll::Ready(Ended::Refused));
    }

    #[test]
    fn transport_error_ends_session() {
        let (broker, mut session) = online(env());
        broker.fail_reads();
        assert_eq!(poll(session.as_mut()), Poll::Ready(Ended::Lost));
    }

    #[test]
    fn unknown_command_keeps_session() {
        let (broker, mut session) = online(env());
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
    fn spin(session: &mut Running, node: &mut crate::ota::tests::Node) {
        for _ in 0..4 {
            assert!(poll(session.as_mut()).is_pending(), "сессия жива");
            assert!(poll(node.as_mut()).is_pending(), "узел OTA не выходит");
        }
    }

    #[test]
    fn retained_reboot_is_ignored() {
        let (ota, mut node) = crate::ota::tests::live();
        let env = env();
        env.ota = Box::leak(Box::new(ota));
        let (broker, mut session) = online(env);
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
        let env = env();
        env.ota = Box::leak(Box::new(ota));
        let (broker, mut session) = online(env);
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

    /// Худший случай [`TX_BUDGET`] влезает ровно в него: на байт меньше — и
    /// исход `reboot` уже не уходит. Так ловится и формула, которая занижает
    /// бюджет, и формула, которая его раздувает.
    #[test]
    fn tx_budget_is_exactly_the_worst_session() {
        assert_eq!(worst_session(TX_BUDGET).as_deref(), Some(&b"accepted"[..]));
        assert_eq!(worst_session(TX_BUDGET - 1), None);
    }

    /// Самые длинные префикс, вход и `info`, ни одного подтверждения от
    /// брокера до исхода `reboot`, буфер передачи `tx` байт. Исход `reboot`,
    /// если он ушёл брокеру.
    fn worst_session(tx: usize) -> Option<Vec<u8>> {
        fn leak(s: std::string::String) -> &'static str {
            Box::leak(s.into_boxed_str())
        }
        let (ota, mut node) = crate::ota::tests::live();
        let env = env();
        env.ota = Box::leak(Box::new(ota));
        env.tx = Box::leak(vec![0; tx].into_boxed_slice());
        let topics = Topics::new(&"p".repeat(PREFIX_MAX), &UID);
        // `info` длиной ровно `INFO_MAX`: версия добирает недостающее.
        let mut info = *env.info;
        let mut json = std::string::String::new();
        crate::device::write_info(&mut json, &info, env.mac, env.ip, Instant::now().as_secs())
            .unwrap();
        info.version = leak("v".repeat(INFO_MAX - json.len() + info.version.len()));
        env.info = Box::leak(Box::new(info));
        let endpoint = Box::leak(Box::new(Endpoint {
            user: leak("u".repeat(AUTH_MAX / 2)),
            password: leak("p".repeat(AUTH_MAX / 2)),
            ..ENDPOINT
        }));
        let reboot = topics.topic::<TOPIC_LEN>(REBOOT);
        let result = topics.reboot_result();
        env.topics = Box::leak(Box::new(topics));
        let broker = FakeBroker::default();
        broker.push(&connack(0));
        let mut session: Running = Box::pin(serve(broker.clone(), endpoint, env));
        // Буфера не хватило — сессия кончается раньше, чем дойдёт до исхода.
        if poll(session.as_mut()).is_ready() {
            return None;
        }
        broker.push(&publish(&reboot, b"", false));
        if poll(session.as_mut()).is_ready() {
            return None;
        }
        assert!(poll(node.as_mut()).is_pending(), "узел ответил");
        if poll(session.as_mut()).is_ready() {
            return None;
        }
        let sent = sent(&broker.output());
        let info = sent.iter().find(|p| p.topic.ends_with("/info"))?;
        assert_eq!(info.payload.len(), INFO_MAX);
        let result = sent.iter().find(|p| p.topic == result.as_str())?;
        Some(result.payload.clone())
    }
    // {%- endif %}

    /// Соединение узла: на каждый `dial` — следующий из заготовленных
    /// сценариев, [`FakeBroker`] или `None` — отказ. Сценарии кончились —
    /// `dial` ждёт вечно, как реализация в паузе перед попыткой.
    #[derive(Clone, Default)]
    struct FakeLink(Rc<RefCell<Calls>>);

    #[derive(Default)]
    struct Calls {
        scenarios: VecDeque<Option<FakeBroker>>,
        current: Option<FakeBroker>,
        dials: usize,
        hang_ups: usize,
    }

    impl FakeLink {
        fn new(scenarios: impl IntoIterator<Item = Option<FakeBroker>>) -> Self {
            let link = Self::default();
            link.0.borrow_mut().scenarios.extend(scenarios);
            link
        }

        fn dials(&self) -> usize {
            self.0.borrow().dials
        }

        fn hang_ups(&self) -> usize {
            self.0.borrow().hang_ups
        }

        fn current(&self) -> FakeBroker {
            self.0.borrow().current.clone().expect("соединение открыто")
        }
    }

    impl Dial for FakeLink {
        type Error = ErrorKind;

        async fn dial(&mut self, to: SocketAddrV4) -> Result<(), ErrorKind> {
            assert_eq!(to, SocketAddrV4::new(ENDPOINT.addr.into(), ENDPOINT.port));
            core::future::poll_fn(|_| {
                let mut calls = self.0.borrow_mut();
                let Some(next) = calls.scenarios.pop_front() else {
                    return Poll::Pending;
                };
                calls.dials += 1;
                calls.current.clone_from(&next);
                Poll::Ready(next.map(drop).ok_or(ErrorKind::TimedOut))
            })
            .await
        }

        fn hang_up(&mut self) {
            self.0.borrow_mut().hang_ups += 1;
        }
    }

    impl ErrorType for FakeLink {
        type Error = ErrorKind;
    }

    impl Read for FakeLink {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, ErrorKind> {
            self.current().read(buf).await
        }
    }

    impl Write for FakeLink {
        async fn write(&mut self, buf: &[u8]) -> Result<usize, ErrorKind> {
            self.current().write(buf).await
        }

        async fn flush(&mut self) -> Result<(), ErrorKind> {
            self.current().flush().await
        }
    }

    /// Брокер, который примет подключение и подписку.
    fn accepting() -> FakeBroker {
        let broker = FakeBroker::default();
        broker.push(&connack(0));
        broker.push(&suback(1));
        broker
    }

    /// Задача узла над `link`: стек с готовым адресом, остальное — на всё
    /// время теста.
    #[expect(
        clippy::too_many_arguments,
        reason = "`ctx()` стенда берёт по аргументу на ресурс узла"
    )]
    fn node(link: FakeLink) -> Pin<Box<dyn Future<Output = TaskExit>>> {
        fn leak<T>(value: T) -> &'static mut T {
            Box::leak(Box::new(value))
        }
        #[expect(
            clippy::default_trait_access,
            reason = "тип списка DNS — heapless из embassy-net, по имени не назван"
        )]
        let config = embassy_net::Config::ipv4_static(embassy_net::StaticConfigV4 {
            address: embassy_net::Ipv4Cidr::new(embassy_net::Ipv4Address::new(10, 0, 0, 2), 24),
            gateway: None,
            dns_servers: Default::default(),
        });
        let (stack, _runner) = embassy_net::new(
            adapters::fake::NetDevice::new([2, 0, 0, 0, 0, 1]),
            config,
            leak(embassy_net::StackResources::<2>::new()),
            1,
        );
        let stand = leak(supervisor::stand!(super::MQTT_FRAG<Fixed, FakeLink>));
        Box::pin(task(stand.ctx(
            leak(stack),
            leak(DeviceInfo {
                version: "1.2.3",
                commit: "abcdef12",
                chip: "stm32f407ve",
                uid: UID,
                flash_size: 512 * 1024,
                sys_hz: None,
            }),
            leak("devices"),
            leak(Fixed(ENDPOINT)),
            leak(link),
            // {%- if ota == "true" %}
            leak(crate::ota::tests::live().0),
            // {%- endif %}
            leak([0; RX_BUFFER]),
            leak([0; TX_BUFFER]),
        )))
    }

    fn spin_node(node: &mut Pin<Box<dyn Future<Output = TaskExit>>>) {
        for _ in 0..8 {
            assert!(poll(node.as_mut()).is_pending(), "узел не выходит");
        }
    }

    #[test]
    fn node_redials_after_refusal() {
        let refusing = FakeBroker::default();
        refusing.push(&connack(0x86));
        let second = accepting();
        let link = FakeLink::new([Some(refusing), Some(second.clone())]);
        let mut node = node(link.clone());
        spin_node(&mut node);
        assert_eq!(link.dials(), 2);
        assert_eq!(link.hang_ups(), 1);
        let sent = sent(&second.output());
        let status = sent.iter().find(|p| p.topic == topic("/status")).unwrap();
        assert_eq!(status.payload, b"online");
    }

    #[test]
    fn node_logs_and_redials_on_dial_failure() {
        let second = accepting();
        let link = FakeLink::new([None, Some(second.clone())]);
        let mut node = node(link.clone());
        spin_node(&mut node);
        assert_eq!(link.dials(), 2);
        let sent = sent(&second.output());
        let status = sent.iter().find(|p| p.topic == topic("/status")).unwrap();
        assert_eq!(status.payload, b"online");
    }

    #[test]
    fn node_redials_after_loss() {
        let (first, second) = (accepting(), accepting());
        let link = FakeLink::new([Some(first.clone()), Some(second.clone())]);
        let mut node = node(link.clone());
        spin_node(&mut node);
        assert_eq!(link.dials(), 1, "первая сессия в сети");
        first.fail_reads();
        spin_node(&mut node);
        assert_eq!(link.dials(), 2);
        assert_eq!(link.hang_ups(), 1);
        assert!(contains(&second.output(), topic("/cmd/#").as_bytes()));
    }

    #[test]
    fn node_never_returns() {
        let link = FakeLink::new([Some(accepting())]);
        let mut node = node(link.clone());
        spin_node(&mut node);
        MockDriver::get().advance(Duration::from_secs(60));
        spin_node(&mut node);
        assert_eq!(link.dials(), 1, "живая сессия не переподключается");
    }
}
