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
//! | `info` | плата → брокер | сведения об устройстве, JSON (retain) |
//! | `telemetry` | плата → брокер | время работы раз в [`TELEMETRY_INTERVAL`](crate::mqtt::TELEMETRY_INTERVAL), JSON |
//! | `cmd/reboot` | брокер → плата | перезагрузка |
//! | `cmd/reboot/result` | плата → брокер | исход команды |
//!
//! Здесь — фрагмент узла, его автомат и задача; имена тем и разбор команд —
//! `topics`, адрес брокера — `broker`, тексты телеметрии — `telemetry`, одна
//! сессия с брокером над готовым потоком байтов — `session` (`serve`), вход —
//! `wiring`.

/// Адрес брокера: `Endpoint` и откуда он берётся (`Broker`, `Fixed`).
mod broker;
/// Сессия `minimq` над готовым потоком байтов и её буферы.
mod session;
/// Как часто и что плата публикует в `telemetry`.
mod telemetry;
/// Имена тем, идентификатор клиента и разбор команд.
mod topics;
/// Вход узла `MqttWiring`.
mod wiring;

use core::net::SocketAddrV4;

use defmt_or_log::{FormatOrDebug, info, warn};
use embedded_io_async::{Read, Write};
use ports::dial::Dial;

pub use self::broker::{Broker, Endpoint, Fixed};
pub use self::session::{AUTH_MAX, CONNECT_TIMEOUT, RX_BUFFER, TX_BUFFER};
use self::session::{Env, serve};
pub use self::telemetry::{TELEMETRY_INTERVAL, write_telemetry};
pub use self::topics::{Command, PREFIX_MAX, Topics, parse_command};
pub use self::wiring::MqttWiring;

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

    node resources: [
            /// Сетевой стек — ради адреса платы и MAC в `info`.
            STACK: local $crate::net::Stack<'static> = inputs.stack,
            /// Сведения для `info`.
            INFO: $crate::device::DeviceInfo = inputs.info,
            /// Префикс тем, не длиннее `PREFIX_MAX`.
            PREFIX: &'static str = inputs.prefix,
            /// Откуда адрес брокера.
            BROKER: local B = inputs.broker,
            /// Соединение с брокером на стеке платы.
            LINK: local D = inputs.link,
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

/// Узел `MQTT`: подключается к брокеру, ведёт сессию, после её конца
/// подключается снова — пока жива прошивка; не возвращается.
///
/// У автомата нет выходного состояния: узел не падает (у него нет
/// `restart:`).
pub async fn task<B: Broker, D: Dial + Read + Write>(mut ctx: MqttCtx<'_, B, D>)
where
    <D as Dial>::Error: FormatOrDebug,
{
    match ctx.run().await {}
}

#[cfg(test)]
pub(crate) mod tests;
