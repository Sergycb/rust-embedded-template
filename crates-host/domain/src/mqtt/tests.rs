//! Тесты узла `MQTT`: темы, телеметрия, сессия над брокером в памяти и
//! задача узла над стендом.

use core::net::SocketAddrV4;
use core::pin::Pin;
use core::task::{Context, Poll, Waker};
use std::cell::RefCell;
use std::collections::VecDeque;
use std::rc::Rc;
use std::vec::Vec;

use embassy_time::{Duration, MockDriver};
use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
use ports::dial::Dial;
use supervisor::runtime::TaskExit;
// {%- if ota == "true" %}
use embassy_time::Instant;
// {%- endif %}

use super::session::{Ended, Env, serve};
use super::{
    CONNECT_TIMEOUT, Command, Endpoint, Fixed, PREFIX_MAX, RX_BUFFER, TELEMETRY_INTERVAL,
    TX_BUFFER, Topics, parse_command, task, write_telemetry,
};
use crate::device::{DeviceInfo, UID_BYTES};
// {%- if ota == "true" %}
use super::AUTH_MAX;
use super::session::TX_BUDGET;
use super::topics::{REBOOT, TOPIC_LEN};
use crate::device::INFO_MAX;
// {%- endif %}

const ENDPOINT: Endpoint = Endpoint {
    addr: [10, 0, 0, 1],
    port: 1883,
    user: "",
    password: "",
};

const UID: [u8; UID_BYTES] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
const UID_TEXT: &str = "0102030405060708090a0b0c";

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

/// Исходящая публикация, разобранная из байтов платы.
#[derive(Debug)]
pub(crate) struct Sent {
    pub(crate) retain: bool,
    pub(crate) topic: std::string::String,
    pub(crate) payload: Vec<u8>,
}

type Running = Pin<Box<dyn Future<Output = Ended>>>;

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
    crate::device::write_info(&mut json, &info, env.mac, env.ip, Instant::now().as_secs()).unwrap();
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
