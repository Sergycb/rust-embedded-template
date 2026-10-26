//! Одна сессия `minimq` с брокером над готовым потоком байтов: подключение с
//! Will, объявление платы, телеметрия и команды, пока связь жива; буферы
//! `minimq` и бюджет передающего.

use defmt_or_log::{info, warn};
use embassy_time::{Duration, Instant, with_deadline, with_timeout};
use heapless::String;
use minimq::{
    Buffers, ConfigBuilder, ConfigError, Connection, Publication, QoS, RetainHandling, Session,
    SubscriptionOptions, TopicFilter, Will,
};
// {%- if ota == "true" %}
use ports::{OtaCommand, Progress};
// {%- endif %}

use super::broker::Endpoint;
use super::telemetry::{TELEMETRY_INTERVAL, write_telemetry};
use super::topics::{CLIENT_ID_LEN, Command, TOPIC_LEN, Topics, parse_command};
use crate::device::{DeviceInfo, INFO_MAX};
// {%- if ota == "true" %}
use crate::ota::OtaClient;
// {%- endif %}

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
    + packet_len(publish_rest(CLIENT_ID_LEN + "/info".len(), INFO_MAX))
    + 5
    + publish_rest(TOPIC_LEN, "accepted".len());
const _: () = assert!(
    TX_BUDGET <= TX_BUFFER,
    "CONNECT и неподтверждённые пакеты не влезают в TX_BUFFER"
);

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
