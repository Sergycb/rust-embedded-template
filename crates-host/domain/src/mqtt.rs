//! Узел `MQTT`: клиент брокера (MQTT 5, `minimq`) поверх сокета
//! стека `embassy-net`.
//!
//! Плата сама подключается к брокеру и держит соединение; все темы лежат под
//! `<prefix>/<uid>/`, где `uid` — заводской номер чипа в hex, а `prefix`
//! задаёт сборка.
//!
//! | Тема | Направление | Содержимое |
//! |---|---|---|
//! | `status` | плата → брокер | `online`/`offline` (retain; `offline` — will) |
//! | `info` | плата → брокер | сведения об устройстве, текст (retain) |
//! | `telemetry` | плата → брокер | время работы раз в [`TELEMETRY_INTERVAL`](crate::mqtt::TELEMETRY_INTERVAL), текст |
//! | `cmd/reboot` | брокер → плата | перезагрузка{% if ota != "true" %}; без OTA — только строка в лог{% endif %} |
// {%- if ota == "true" %}
//! | `cmd/reboot/result` | плата → брокер | исход команды |
// {%- endif %}
//!
//! Здесь — фрагмент узла, его цикл и задача; имена тем и разбор команд —
//! `topics`, адрес брокера — `broker`, TCP-клиент брокера с паузами между
//! попытками — `client` ([`Tcp`](crate::mqtt::Tcp)), тексты
//! телеметрии — `telemetry`, одна сессия с брокером над готовым потоком
//! байтов — `session` (`serve`). Вход узла (`MqttWiring`) выводит фрагмент.

/// Адрес брокера: `Broker`.
mod broker;
/// TCP-клиент брокера над сокетом стека: подключение с паузой между попытками.
mod client;
/// Сессия `minimq` над готовым потоком байтов и её буферы.
mod session;
/// Как часто и что плата публикует в `telemetry`.
mod telemetry;
/// Имена тем, идентификатор клиента и разбор команд.
mod topics;

use core::net::SocketAddrV4;

use defmt_or_log::{info, warn};

pub use self::broker::Broker;
pub use self::client::{Error, Tcp};
pub use self::session::{AUTH_MAX, CONNECT_TIMEOUT, RX_BUFFER, TX_BUFFER};
use self::session::{Env, serve};
pub use self::telemetry::{TELEMETRY_INTERVAL, write_telemetry};
pub use self::topics::{Command, PREFIX_MAX, Topics, parse_command};

// Узел объявлен рядом со своей задачей. Адрес брокера — во входе,
// соединение — `Tcp` над сокетом стека (его слот `LINK` собирает
// compose-site из стека и буферов TCP от `app`); `local` у него и у стека —
// оба вправе быть `!Send`.
// Буферы `minimq` — слоты `= static`: через стек они не идут. Без
// `watchdog:`: брокера может не быть всю жизнь платы.
supervisor::supervisor_fragment! {
    name: MQTT_FRAG;
    boot: inputs: struct $crate::mqtt::MqttWiring;

    node resources: [
            /// Сетевой стек — ради адреса платы и MAC в `info`.
            STACK: local $crate::net::Stack<'static> = inputs.stack,
            /// Сведения для `info`.
            INFO: $crate::device::DeviceInfo = inputs.info,
            /// Префикс тем, не длиннее `PREFIX_MAX`.
            PREFIX: &'static str = inputs.prefix,
            /// Адрес брокера.
            BROKER: $crate::mqtt::Broker = inputs.broker,
            /// Соединение с брокером на стеке платы.
            LINK: local $crate::mqtt::Tcp = inputs.link,
            // {%- if ota == "true" %}
            /// Клиент входа `CMD` узла OTA — для `cmd/reboot`.
            OTA: $crate::ota::Client = inputs.ota,
            // {%- endif %}
            /// Входящие пакеты `minimq`.
            MQTT_RX: &'static mut [u8; $crate::mqtt::RX_BUFFER] = static,
            /// Исходящие пакеты `minimq`.
            MQTT_TX: &'static mut [u8; $crate::mqtt::TX_BUFFER] = static
        ],
        ctx: $crate::mqtt::MqttCtx,
        task: $crate::mqtt::task;
}

/// Узел `MQTT`: подключается к брокеру, ведёт сессию, после её конца
/// подключается снова — пока жива прошивка; не возвращается.
///
/// У узла нет выхода: узел не падает (у него нет `restart:`).
pub async fn task(ctx: MqttCtx<'_>) {
    info!("mqtt: узел запущен");
    // Первое подключение — после того, как стек получил адрес
    // (статический или от DHCP).
    ctx.stack.wait_config_up().await;
    let topics = Topics::new(ctx.prefix, &ctx.info.uid);
    loop {
        // Адрес брокера и одна попытка подключиться к нему; паузу перед ней
        // выдерживает клиент (`Tcp::dial`).
        let endpoint = *ctx.broker;
        match ctx
            .link
            .dial(SocketAddrV4::new(endpoint.addr.into(), endpoint.port))
            .await
        {
            Ok(()) => {
                // Среда в стеке одна — Ethernet (`medium-ethernet`).
                let embassy_net::HardwareAddress::Ethernet(mac) = ctx.stack.hardware_address();
                let mut env = Env {
                    topics: &topics,
                    info: ctx.info,
                    mac: mac.0,
                    ip: ctx.stack.config_v4().map(|config| config.address.address()),
                    rx: &mut ctx.mqtt_rx[..],
                    tx: &mut ctx.mqtt_tx[..],
                    // {%- if ota == "true" %}
                    ota: ctx.ota,
                    // {%- endif %}
                };
                serve(&mut *ctx.link, &endpoint, &mut env).await;
                // Чем бы сессия ни кончилась (строку лога пишет `serve`),
                // трубка кладётся, и узел подключается снова.
                ctx.link.hang_up();
            }
            Err(err) => {
                // Брокер недоступен — строка в лог на каждую попытку:
                // адрес-заглушка из `mqtt.toml` иначе молчал бы.
                let [a, b, c, d] = endpoint.addr;
                warn!(
                    "mqtt: брокер {}.{}.{}.{}:{} недоступен ({:?}) — проверьте mqtt.toml",
                    a, b, c, d, endpoint.port, err
                );
            }
        }
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! Тесты узла `MQTT`: темы, телеметрия, сессия над брокером в памяти и
    //! задача узла над стендом.

    use core::pin::Pin;
    use core::task::{Context, Poll, Waker};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;
    use std::vec::Vec;

    use adapters_hw::net::fake::cable::{leak, pair};
    use embassy_net::Stack;
    use embassy_net::tcp::TcpSocket;
    use embassy_time::{Duration, MockDriver};
    use embedded_io_async::{ErrorKind, ErrorType, Read, Write};
    // {%- if ota == "true" %}
    use embassy_time::Instant;
    // {%- endif %}

    use super::session::{Ended, Env, serve};
    use super::{
        Broker, CONNECT_TIMEOUT, Command, PREFIX_MAX, RX_BUFFER, TELEMETRY_INTERVAL, TX_BUFFER,
        Tcp, Topics, parse_command, task, write_telemetry,
    };
    use crate::device::{DeviceInfo, UID_BYTES};
    // {%- if ota == "true" %}
    use super::AUTH_MAX;
    use super::session::TX_BUDGET;
    use super::topics::{REBOOT, TOPIC_LEN};
    use crate::device::INFO_TEXT;
    // {%- endif %}

    /// Брокер — на «хосте» кабеля в памяти (`adapters_hw::net::fake::cable::HOST`).
    const ENDPOINT: Broker = Broker {
        addr: [10, 0, 0, 2],
        port: 1883,
        user: "",
        password: "",
    };

    const UID: [u8; UID_BYTES] = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12];
    const UID_TEXT: &str = "0102030405060708090a0b0c";

    /// Отложенное подтверждение `smoltcp`: при стоящем времени оно не уходит
    /// никогда, и `flush` клиента ждал бы вечно. Ждущая сторона двигает время
    /// на эту паузу.
    const ACK_DELAY: Duration = Duration::from_millis(10);

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

    /// Брокер на «хосте» кабеля: сокет, на котором он принимает плату.
    struct HostBroker(TcpSocket<'static>);

    impl HostBroker {
        fn new(host: Stack<'static>) -> Self {
            Self(TcpSocket::new(host, leak([0; 2048]), leak([0; 2048])))
        }

        /// Принимает подключение платы и читает её CONNECT.
        async fn accept(&mut self) -> Vec<u8> {
            pump(self.0.accept(ENDPOINT.port))
                .await
                .expect("плата подключилась");
            let mut got = Vec::new();
            self.read_until(&mut got, b"MQTT").await;
            got
        }

        /// Читает в `got`, пока там не найдётся `needle`.
        async fn read_until(&mut self, got: &mut Vec<u8>, needle: &[u8]) {
            while !contains(got, needle) {
                let mut buf = [0; 512];
                let n = pump(self.0.read(&mut buf)).await.expect("соединение живо");
                assert!(n > 0, "плата закрыла соединение раньше {needle:02X?}");
                got.extend_from_slice(&buf[..n]);
            }
        }

        /// Пакет брокера плате.
        async fn send(&mut self, bytes: &[u8]) {
            pump(self.0.write_all(bytes)).await.expect("пакет ушёл");
        }

        /// Подключение платы закрыто — ею (трубка положена) или брокером
        /// (`abort`): сокет снова свободен для `accept`.
        async fn close(&mut self) {
            self.0.abort();
            pump(self.0.flush()).await.ok();
        }

        /// Принимает плату и ведёт её до подписки: CONNACK, SUBACK; отдаёт
        /// всё, что плата прислала до публикации `online`.
        async fn welcome(&mut self) -> Vec<u8> {
            let mut got = self.accept().await;
            self.send(&connack(0)).await;
            self.read_until(&mut got, topic("/cmd/#").as_bytes()).await;
            self.send(&suback(1)).await;
            self.read_until(&mut got, b"online").await;
            got
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
    fn telemetry_is_text() {
        let mut s = std::string::String::new();
        write_telemetry(&mut s, 42).unwrap();
        assert_eq!(s, "uptime_s 42\n");
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
                reset: "pin",
                last_panic: None,
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
        assert!(contains(&info.payload, b"chip "));
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
        assert!(telemetry.payload.starts_with(b"uptime_s "));
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

    /// Отказ брокера в ответ на публикацию (PUBACK с кодом 0x87, Not
    /// authorized — ACL не пускает в тему) — не обрыв связи: сессия живёт.
    #[test]
    fn rejected_publish_keeps_session() {
        let (broker, mut session) = online(env());
        let [hi, lo] = 2u16.to_be_bytes();
        broker.push(&[0x40, 0x03, hi, lo, 0x87]);
        assert!(poll(session.as_mut()).is_pending(), "сессия в сети");
        MockDriver::get().advance(TELEMETRY_INTERVAL);
        assert!(poll(session.as_mut()).is_pending());
        let sent = sent(&broker.output());
        assert!(sent.iter().any(|p| p.topic == topic("/telemetry")));
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
        // `info` длиной ровно `INFO_TEXT`: версия добирает недостающее.
        let mut info = *env.info;
        let mut text = std::string::String::new();
        crate::device::write_info(
            &mut text,
            &info,
            Some(env.mac),
            env.ip.map(|ip| core::net::Ipv4Addr::from(ip.octets())),
            Instant::now().as_secs(),
        )
        .unwrap();
        info.version = leak("v".repeat(INFO_TEXT - text.len() + info.version.len()));
        env.info = Box::leak(Box::new(info));
        let endpoint = Box::leak(Box::new(Broker {
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
        assert_eq!(info.payload.len(), INFO_TEXT);
        let result = sent.iter().find(|p| p.topic == result.as_str())?;
        Some(result.payload.clone())
    }
    // {%- endif %}

    /// Ждёт `fut`, двигая время на [`ACK_DELAY`], пока он не готов за круг
    /// опроса остальных: так уходят подтверждения стека и выдерживаются паузы
    /// клиента перед попыткой.
    async fn pump<T>(fut: impl Future<Output = T>) -> T {
        let mut fut = core::pin::pin!(fut);
        loop {
            tokio::select! {
                biased;
                out = &mut fut => return out,
                () = tokio::task::yield_now() => MockDriver::get().advance(ACK_DELAY),
            }
        }
    }

    /// Задача узла над клиентом брокера на стеке `stack`, остальное — на
    /// всё время теста.
    #[expect(
        clippy::too_many_arguments,
        reason = "`ctx()` стенда берёт по аргументу на ресурс узла"
    )]
    fn node(stack: Stack<'static>) -> Pin<Box<dyn Future<Output = ()>>> {
        let stand = leak(supervisor::stand!(super::MQTT_FRAG));
        Box::pin(task(stand.ctx(
            leak(stack),
            leak(DeviceInfo {
                version: "1.2.3",
                commit: "abcdef12",
                chip: "stm32f407ve",
                uid: UID,
                flash_size: 512 * 1024,
                sys_hz: None,
                reset: "pin",
                last_panic: None,
            }),
            leak("devices"),
            leak(ENDPOINT),
            leak(Tcp::new(stack, leak([0; 1024]), leak([0; 1024]))),
            // {%- if ota == "true" %}
            leak(crate::ota::tests::live().0),
            // {%- endif %}
            leak([0; RX_BUFFER]),
            leak([0; TX_BUFFER]),
        )))
    }

    /// Гоняет оба стека и узел на «устройстве», пока брокер ведёт сценарий
    /// `script` на «хосте».
    async fn with_broker<R>(script: impl AsyncFnOnce(HostBroker) -> R) -> R {
        MockDriver::get().reset();
        let ((device, mut device_runner), (host, mut host_runner)) = pair();
        let node = node(device);
        tokio::select! {
            never = device_runner.run() => never,
            never = host_runner.run() => never,
            () = node => panic!("узел вернулся"),
            result = script(HostBroker::new(host)) => result,
        }
    }

    /// Брокер отверг подключение — плата кладёт трубку и подключается снова
    /// после паузы клиента (соединение прожило меньше 30 с — отказ), и вторая
    /// сессия выходит в сеть.
    #[tokio::test]
    async fn node_redials_after_refusal() {
        with_broker(async |mut broker| {
            broker.accept().await;
            broker.send(&connack(0x86)).await;
            broker.close().await;
            let refused = embassy_time::Instant::now();
            let second = broker.welcome().await;
            assert!(
                refused.elapsed() >= Duration::from_secs(1),
                "повтор без паузы клиента"
            );
            assert!(contains(&second, b"online"));
        })
        .await;
    }

    /// Брокера ещё нет — попытка отказывает (RST), узел пишет в лог и
    /// подключается снова, когда брокер поднялся.
    #[tokio::test]
    async fn node_logs_and_redials_on_dial_failure() {
        with_broker(async |mut broker| {
            // Пусть первая попытка придёт, пока никто не слушает.
            for _ in 0..100 {
                tokio::task::yield_now().await;
            }
            let started = embassy_time::Instant::now();
            let session = broker.welcome().await;
            // Подключение — только после паузы клиента: значит, первая
            // попытка отказала, а не дождалась брокера.
            assert!(
                started.elapsed() >= Duration::from_secs(1),
                "первая попытка не отказала"
            );
            assert!(contains(&session, topic("/status").as_bytes()));
        })
        .await;
    }

    /// Связь оборвалась посреди сессии — узел подключается снова и снова
    /// подписывается.
    #[tokio::test]
    async fn node_redials_after_loss() {
        with_broker(async |mut broker| {
            broker.welcome().await;
            broker.close().await;
            let second = broker.welcome().await;
            assert!(contains(&second, topic("/cmd/#").as_bytes()));
        })
        .await;
    }

    /// Минута в сети — узел не возвращается (`with_broker` упал бы на его
    /// выходе) и живую сессию не рвёт.
    #[tokio::test]
    async fn node_never_returns() {
        with_broker(async |mut broker| {
            broker.welcome().await;
            for _ in 0..60 {
                MockDriver::get().advance(Duration::from_secs(1));
                for _ in 0..10 {
                    tokio::task::yield_now().await;
                }
            }
            assert_eq!(
                broker.0.state(),
                embassy_net::tcp::State::Established,
                "сессия та же"
            );
        })
        .await;
    }
}
