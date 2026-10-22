//! Узел `MQTT`: клиент брокера. То, что в графе держат слоты `= static`, —
//! буферы `minimq` — здесь статики; буферы сокета — у `crate::net::mqtt_buffers`.

use domain::mqtt::{RX_BUFFER, TX_BUFFER};
use static_cell::ConstStaticCell;

static MQTT_RX: ConstStaticCell<[u8; RX_BUFFER]> = ConstStaticCell::new([0; RX_BUFFER]);
static MQTT_TX: ConstStaticCell<[u8; TX_BUFFER]> = ConstStaticCell::new([0; TX_BUFFER]);

#[embassy_executor::task]
async fn task(mut stack: bsp::net::Stack, mut info: domain::device::DeviceInfo) {
    let mut prefix = crate::net::MQTT_PREFIX;
    let mut broker = domain::mqtt::Fixed(crate::net::MQTT_BROKER);
    // Соединение — на стеке платы, как в слоте фрагмента `MQTT_FRAG`.
    let mut link =
        <adapters::net::TcpClient as domain::Assemble>::assemble(stack, crate::net::mqtt_buffers());
{%- if ota == "true" %}
    let cmd = crate::tasks::ota::CMD.requester();
    let mut ota = domain::ota::OtaClient::new(cmd, domain::ota::ClientId({{ mqtt_ota_client }}));
{%- endif %}
    domain::mqtt::task(domain::mqtt::MqttCtx {
        stack: &mut stack,
        info: &mut info,
        prefix: &mut prefix,
        broker: &mut broker,
        link: &mut link,
{%- if ota == "true" %}
        ota: &mut ota,
{%- endif %}
        mqtt_rx: MQTT_RX.take(),
        mqtt_tx: MQTT_TX.take(),
        index: 0,
    })
    .await;
}

/// Спавнит узел.
pub fn spawn(
    spawner: embassy_executor::Spawner,
    stack: bsp::net::Stack,
    info: domain::device::DeviceInfo,
) {
    spawner.spawn(task(stack, info).expect("задача MQTT свежая: спавнится один раз"));
}
