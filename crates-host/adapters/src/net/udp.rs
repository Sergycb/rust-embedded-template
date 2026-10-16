//! UDP-канал на одном порту — [`FrameLink`] для протоколов, которым
//! датаграмма и есть кадр.

use embassy_net::Stack;
use embassy_net::udp::{PacketMetadata, RecvError, SendError, UdpMetadata, UdpSocket};
use ports::{FrameLink, LinkError};

/// UDP-канал на одном порту — [`FrameLink`] для протоколов, которым
/// датаграмма и есть кадр (`modbus::Udp`, узел HOST).
///
/// Отвечает тому, кто прислал последнюю датаграмму: адрес отправителя порт
/// не передаёт, а протоколы отвечают на запрос.
pub struct UdpLink {
    socket: UdpSocket<'static>,
    peer: Option<UdpMetadata>,
}

// Вручную: у сокетов `embassy-net` нет `Debug`.
impl core::fmt::Debug for UdpLink {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("UdpLink").finish_non_exhaustive()
    }
}

impl UdpLink {
    /// Канал на порту `port` стека `stack`: буферы приёма и передачи и их
    /// метаданные — по датаграмме на запись.
    ///
    /// # Panics
    ///
    /// Если `port` 0 — сокет без порта не принимает ничего.
    #[must_use]
    pub fn new(
        stack: Stack<'static>,
        port: u16,
        rx_meta: &'static mut [PacketMetadata],
        rx: &'static mut [u8],
        tx_meta: &'static mut [PacketMetadata],
        tx: &'static mut [u8],
    ) -> Self {
        assert!(port != 0, "UDP-канал на порту 0");
        let mut socket = UdpSocket::new(stack, rx_meta, rx, tx_meta, tx);
        // Свежий сокет с ненулевым портом привязывается всегда.
        let bound = socket.bind(port);
        assert!(bound.is_ok(), "привязка UDP к порту {port}: {bound:?}");
        Self { socket, peer: None }
    }
}

/// Почему [`UdpLink`] потерял датаграмму.
#[derive(Debug)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum UdpError {
    /// Датаграмма длиннее буфера.
    Truncated,
    /// Отвечать некому: датаграмм ещё не было.
    NoPeer,
    /// Стек не отправил датаграмму.
    Send(SendError),
}

impl FrameLink for UdpLink {
    type Error = UdpError;

    async fn recv(&mut self, buf: &mut [u8]) -> Result<usize, LinkError<UdpError>> {
        match self.socket.recv_from(buf).await {
            Ok((len, meta)) => {
                self.peer = Some(meta);
                Ok(len)
            }
            Err(RecvError::Truncated) => Err(LinkError::Lost(UdpError::Truncated)),
        }
    }

    async fn send(&mut self, frame: &[u8]) -> Result<(), LinkError<UdpError>> {
        let Some(peer) = self.peer else {
            return Err(LinkError::Lost(UdpError::NoPeer));
        };
        self.socket
            .send_to(frame, peer)
            .await
            .map_err(|err| LinkError::Lost(UdpError::Send(err)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::net::fakes::{DEVICE, PORT, leak, pair, with_network};

    fn udp(stack: Stack<'static>, port: u16) -> UdpLink {
        UdpLink::new(
            stack,
            port,
            leak([PacketMetadata::EMPTY; 4]),
            leak([0; 16]),
            leak([PacketMetadata::EMPTY; 4]),
            leak([0; 64]),
        )
    }

    /// UDP-канал отвечает отправителю последней датаграммы; ответить до
    /// первой датаграммы некому, длинная датаграмма — потеря.
    #[tokio::test]
    async fn udp_link_replies_to_the_last_sender() {
        let ((device, device_runner), (host, host_runner)) = pair();
        let mut link = udp(device, PORT);
        let peer = udp(host, 4000);
        with_network(device_runner, host_runner, async {
            assert!(matches!(
                link.send(b"x").await,
                Err(LinkError::Lost(UdpError::NoPeer))
            ));

            peer.socket
                .send_to(&[1, 2, 3], (DEVICE, PORT))
                .await
                .unwrap();
            let mut buf = [0; 8];
            assert_eq!(link.recv(&mut buf).await.unwrap(), 3);
            assert_eq!(buf[..3], [1, 2, 3]);
            link.send(&[9, 8]).await.unwrap();
            let mut reply = [0; 16];
            let (n, from) = peer.socket.recv_from(&mut reply).await.unwrap();
            assert_eq!((&reply[..n], from.endpoint.port), (&[9, 8][..], PORT));

            peer.socket.send_to(&[0; 12], (DEVICE, PORT)).await.unwrap();
            let mut short = [0; 4];
            assert!(matches!(
                link.recv(&mut short).await,
                Err(LinkError::Lost(UdpError::Truncated))
            ));
        })
        .await;
    }
}
