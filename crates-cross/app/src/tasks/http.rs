//! Узлы `HTTP`: REST API, пул по задаче на соединение
//! (`crate::net::rest::CONNECTIONS`). То, что в графе держат слоты `= static`, —
//! буферы соединения — здесь статики, по одному набору на копию.

use domain::http::{HTTP_BUFFER, TCP_RX, TCP_TX};
use static_cell::ConstStaticCell;

use crate::net::rest::CONNECTIONS;

/// Буферы одной копии: приём и передача сокета, разбор запроса. Нули —
/// `.bss`, а не образ во флеше.
struct Buffers {
    rx: ConstStaticCell<[u8; TCP_RX]>,
    tx: ConstStaticCell<[u8; TCP_TX]>,
    http: ConstStaticCell<[u8; HTTP_BUFFER]>,
}

static BUFFERS: [Buffers; CONNECTIONS] = [const {
    Buffers {
        rx: ConstStaticCell::new([0; TCP_RX]),
        tx: ConstStaticCell::new([0; TCP_TX]),
        http: ConstStaticCell::new([0; HTTP_BUFFER]),
    }
}; CONNECTIONS];

#[embassy_executor::task(pool_size = CONNECTIONS)]
async fn task(index: usize, mut stack: bsp::net::Stack, mut info: domain::device::DeviceInfo) {
    let buffers = &BUFFERS[index];
    let mut token = crate::net::rest::TOKEN;
{%- if ota == "true" %}
    // Перезагрузку копия просит у узла OTA своим клиентом — номера следом за
    // транспортами, как в графе.
    let cmd = crate::tasks::ota::CMD.requester();
    let id = {% if rest_ota_client != "0" %}{{ rest_ota_client }} + {% endif %}u8::try_from(index).expect("копий пула HTTP меньше 256");
    let mut ota = domain::ota::OtaClient::new(cmd, domain::ota::ClientId(id));
{%- endif %}
    domain::http::task(domain::http::HttpCtx {
        stack: &mut stack,
        info: &mut info,
        token: &mut token,
{%- if ota == "true" %}
        ota: &mut ota,
{%- endif %}
        rx: buffers.rx.take(),
        tx: buffers.tx.take(),
        http: buffers.http.take(),
        index,
    })
    .await;
}

/// Спавнит пул.
pub fn spawn(
    spawner: embassy_executor::Spawner,
    stack: bsp::net::Stack,
    info: domain::device::DeviceInfo,
) {
    for index in 0..CONNECTIONS {
        spawner
            .spawn(task(index, stack, info).expect("задача HTTP свежая: пул спавнится один раз"));
    }
}
