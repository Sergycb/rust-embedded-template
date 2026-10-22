//! Узел `SSH`: консоль платы. То, что в графе держат слоты `= static`, —
//! буферы соединения — здесь статики.

use domain::ssh::{SSH_BUFFER, TCP_BUFFER};
use static_cell::ConstStaticCell;

/// Ключ хоста узла и то, из чего он собирается.
{%- if ssh_host_key == "config" %}
type HostKey = domain::ssh::Stored<crate::settings::Settings>;
type HostKeySource = bsp::settings::ConfigFlash;
{%- else %}
type HostKey = domain::ssh::Fixed;
type HostKeySource = &'static [u8; 32];
{%- endif %}

static TCP_RX: ConstStaticCell<[u8; TCP_BUFFER]> = ConstStaticCell::new([0; TCP_BUFFER]);
static TCP_TX: ConstStaticCell<[u8; TCP_BUFFER]> = ConstStaticCell::new([0; TCP_BUFFER]);
static SSH_RX: ConstStaticCell<[u8; SSH_BUFFER]> = ConstStaticCell::new([0; SSH_BUFFER]);
static SSH_TX: ConstStaticCell<[u8; SSH_BUFFER]> = ConstStaticCell::new([0; SSH_BUFFER]);

#[embassy_executor::task]
async fn task(
    mut stack: bsp::net::Stack,
    mut info: domain::device::DeviceInfo,
    host_key: HostKeySource,
) {
    // Ключ хоста — как в слоте фрагмента `SSH_FRAG`.
    let mut host_key = <HostKey as domain::Assemble>::assemble(host_key, ());
    let mut login = crate::net::SSH_LOGIN;
{%- if ota == "true" %}
    let cmd = crate::tasks::ota::CMD.requester();
    let mut ota = domain::ota::OtaClient::new(cmd, domain::ota::ClientId({{ ssh_ota_client }}));
{%- endif %}
    domain::ssh::task(domain::ssh::SshCtx {
        stack: &mut stack,
        info: &mut info,
        login: &mut login,
        host_key: &mut host_key,
{%- if ota == "true" %}
        ota: &mut ota,
{%- endif %}
        tcp_rx: TCP_RX.take(),
        tcp_tx: TCP_TX.take(),
        ssh_rx: SSH_RX.take(),
        ssh_tx: SSH_TX.take(),
        index: 0,
    })
    .await;
}

/// Спавнит узел.
pub fn spawn(
    spawner: embassy_executor::Spawner,
    stack: bsp::net::Stack,
    info: domain::device::DeviceInfo,
    host_key: HostKeySource,
) {
    spawner.spawn(task(stack, info, host_key).expect("задача SSH свежая: спавнится один раз"));
}
