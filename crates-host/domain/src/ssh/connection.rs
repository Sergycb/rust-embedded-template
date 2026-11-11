//! Одно SSH-соединение: сокет, протокол и сессия — до конца соединения.

use defmt_or_log::{info, warn};
use embassy_net::tcp::TcpSocket;
use embassy_time::with_timeout;
use sunset_async::SSHServer;
use sunset_async::sunset::SignKey;

use super::session::{CLOSE_WAIT, IDLE, KEEP_ALIVE, PORT, SSH_BUFFER, TCP_BUFFER, session};
use crate::console::{Access, Device};
use crate::device::DeviceInfo;
// {%- if ota == "true" %}
use crate::ota::OtaClient;
// {%- else %}
//{{newline}}use super::ota_stub::OtaClient;
// {%- endif %}

/// Одно соединение и всё, что ему нужно на весь его срок.
///
/// Поля — из контекста узла: задача забирает их один раз, а не передаёт
/// контекст в логику.
pub(super) struct Connection<'a> {
    /// Сетевой стек, на котором открывается сокет.
    stack: crate::net::Stack<'static>,
    /// Сведения для `info`.
    info: &'a DeviceInfo,
    /// Клиент входа `CMD` узла OTA; без OTA — заглушка, поле не читается.
    #[allow(dead_code, reason = "без OTA поле не читается, но тип общий")]
    ota: OtaClient,
    /// Приём сокета.
    tcp_rx: &'a mut [u8; TCP_BUFFER],
    /// Передача сокета.
    tcp_tx: &'a mut [u8; TCP_BUFFER],
    /// Пакеты SSH от клиента.
    ssh_rx: &'a mut [u8; SSH_BUFFER],
    /// Пакеты SSH клиенту.
    ssh_tx: &'a mut [u8; SSH_BUFFER],
}

impl<'a> Connection<'a> {
    /// Собирает соединение из полей контекста узла.
    #[must_use]
    pub(super) fn new(
        stack: crate::net::Stack<'static>,
        info: &'a DeviceInfo,
        ota: OtaClient,
        tcp_rx: &'a mut [u8; TCP_BUFFER],
        tcp_tx: &'a mut [u8; TCP_BUFFER],
        ssh_rx: &'a mut [u8; SSH_BUFFER],
        ssh_tx: &'a mut [u8; SSH_BUFFER],
    ) -> Self {
        Self {
            stack,
            info,
            ota,
            tcp_rx,
            tcp_tx,
            ssh_rx,
            ssh_tx,
        }
    }

    /// Одно соединение. Отказ соединения или протокола узел пишет в лог и
    /// слушает дальше.
    pub(super) async fn run(&mut self, key: &SignKey, access: Access) {
        let stack = self.stack;
        let mut socket = TcpSocket::new(stack, &mut self.tcp_rx[..], &mut self.tcp_tx[..]);
        socket.set_timeout(Some(IDLE));
        socket.set_keep_alive(Some(KEEP_ALIVE));
        if socket.accept(PORT).await.is_err() {
            warn!("ssh: accept не удался");
            return;
        }
        if let Some(peer) = socket.remote_endpoint() {
            info!("ssh: соединение от {}", peer);
        }
        let device = Device {
            info: self.info,
            stack: Some(stack),
            // {%- if ota == "true" %}
            ota: self.ota,
            // {%- endif %}
        };
        session(
            &mut socket,
            SSHServer::new(&mut self.ssh_rx[..], &mut self.ssh_tx[..]),
            key,
            access,
            &device,
        )
        .await;
        // Соединение закрывает сервер: `sunset` закрывает канал, но не
        // соединение, а клиент ждёт именно его. Сокет бросается только после
        // FIN клиента: брошенный раньше ответит на него RST, и клиент под
        // Windows теряет ещё не прочитанный вывод.
        socket.close();
        // Хвост вывода и наш FIN уходят раньше, чем ждётся FIN клиента:
        // сокет, брошенный с неподтверждёнными данными, их не досылает.
        let drained = with_timeout(CLOSE_WAIT, async {
            socket.flush().await.ok();
            let mut sink = [0; 64];
            while let Ok(1..) = socket.read(&mut sink).await {}
        })
        .await;
        if drained.is_err() {
            socket.abort();
        }
        info!("ssh: соединение закрыто");
    }
}
