//! Modbus UDP: заголовок MBAP и PDU, по кадру в датаграмме.

use ports::{
    FrameLink, LinkError, MAX_PDU, MasterError, ModbusClient, ModbusServer, Received, ServeError,
};

use super::mbap::{self, HEADER, Header, MAX_ADU};

/// Сколько потерянных датаграмм подряд терпит приём, прежде чем объявить
/// канал отказавшим: серия потерь — уже не помеха, и узлу о ней стоит знать.
/// Переждать отказ — дело канала под транспортом (`ports::FrameLink`).
const MAX_LOST_IN_A_ROW: u8 = 32;

/// Modbus UDP поверх датаграмм `link` (UDP-сокет `embassy-net` за
/// [`FrameLink`]).
///
/// Отвечать slave'у некуда, кроме как отправителю последнего запроса: этот
/// адрес помнит реализация `link`, порт его не передаёт.
///
/// Битая датаграмма (короче заголовка, длина в MBAP не совпала с
/// датаграммой, чужой протокол) пропускается, master пропускает и ответ с
/// чужим номером транзакции. Потерянный ответ slave'а
/// ([`LinkError::Lost`] на отправке) — как помеха на линии: master
/// повторит запрос. Slave получает любой запрос как адресованный ему, 0xFF
/// (см. [модуль](super)).
#[derive(Debug)]
pub struct Udp<L> {
    link: L,
    /// Датаграмма — принятая или собираемая к отправке.
    buf: [u8; MAX_ADU],
    /// Номер транзакции: master — последнего запроса, slave — последнего
    /// принятого (им подписан ответ).
    tid: u16,
    /// Адрес из последнего принятого запроса — slave повторяет его в ответе.
    unit: u8,
}

impl<L> Udp<L> {
    /// Транспорт поверх датаграмм `link`.
    pub const fn new(link: L) -> Self {
        Self {
            link,
            buf: [0; MAX_ADU],
            tid: 0,
            unit: 0,
        }
    }
}

impl<L: FrameLink> Udp<L> {
    /// Ждёт целую датаграмму Modbus в `buf`; возвращает заголовок и длину.
    async fn datagram(&mut self) -> Result<(Header, usize), LinkError<L::Error>> {
        let mut lost = 0_u8;
        loop {
            match self.link.recv(&mut self.buf).await {
                Ok(len) if len >= HEADER => {
                    let header = Header::parse(&self.buf);
                    if header.protocol == 0 && header.total() == Some(len) {
                        return Ok((header, len));
                    }
                }
                Ok(_) => {}
                Err(LinkError::Lost(e)) => {
                    lost += 1;
                    if lost >= MAX_LOST_IN_A_ROW {
                        return Err(LinkError::Lost(e));
                    }
                }
                Err(down) => return Err(down),
            }
        }
    }

    /// Собирает датаграмму и отправляет.
    async fn send(&mut self, tid: u16, unit: u8, pdu: &[u8]) -> Result<(), LinkError<L::Error>> {
        self.buf[..HEADER].copy_from_slice(&mbap::head(tid, unit, pdu.len()));
        self.buf[HEADER..HEADER + pdu.len()].copy_from_slice(pdu);
        self.link.send(&self.buf[..HEADER + pdu.len()]).await
    }
}

impl<L: FrameLink> ModbusServer for Udp<L> {
    type Error = LinkError<L::Error>;

    async fn recv(&mut self, buf: &mut [u8]) -> Result<Received, ServeError<Self::Error>> {
        assert!(buf.len() >= MAX_PDU, "буфер PDU {} < {MAX_PDU}", buf.len());
        let (header, len) = self.datagram().await.map_err(ServeError::Io)?;
        let pdu = &self.buf[HEADER..len];
        buf[..pdu.len()].copy_from_slice(pdu);
        self.tid = header.tid;
        self.unit = header.unit;
        Ok(Received {
            unit: 0xFF,
            len: pdu.len(),
        })
    }

    async fn reply(&mut self, response: &[u8]) -> Result<(), ServeError<Self::Error>> {
        match self.send(self.tid, self.unit, response).await {
            Ok(()) | Err(LinkError::Lost(_)) => Ok(()),
            Err(down) => Err(ServeError::Io(down)),
        }
    }
}

impl<L: FrameLink> ModbusClient for Udp<L> {
    type Error = LinkError<L::Error>;

    async fn exchange(
        &mut self,
        unit: u8,
        request: &[u8],
        response: &mut [u8],
    ) -> Result<usize, MasterError<Self::Error>> {
        assert!(
            response.len() >= MAX_PDU,
            "буфер PDU {} < {MAX_PDU}",
            response.len()
        );
        self.tid = self.tid.wrapping_add(1);
        self.send(self.tid, unit, request)
            .await
            .map_err(MasterError::Io)?;
        loop {
            let (header, len) = self.datagram().await.map_err(MasterError::Io)?;
            if header.tid != self.tid {
                // Опоздавший ответ на брошенный запрос.
                continue;
            }
            if header.unit != unit {
                return Err(MasterError::Unexpected);
            }
            let pdu = &self.buf[HEADER..len];
            response[..pdu.len()].copy_from_slice(pdu);
            return Ok(pdu.len());
        }
    }

    async fn broadcast(&mut self, request: &[u8]) -> Result<(), MasterError<Self::Error>> {
        self.tid = self.tid.wrapping_add(1);
        self.send(self.tid, 0, request)
            .await
            .map_err(MasterError::Io)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::modbus::fakes::{Datagrams, mbap_frame as frame};

    #[tokio::test]
    async fn server_skips_broken_and_replies_to_last() {
        let mut bad_length = frame(3, 0, 1, &[0x03, 0, 0, 0, 1]);
        bad_length[5] = 9;
        let mut udp = Udp::new(Datagrams::with([
            Ok(vec![0, 1, 0]),
            Ok(frame(2, 7, 1, &[0x03, 0, 0, 0, 1])),
            Ok(bad_length),
            Err(LinkError::Lost("обрезана")),
            Ok(frame(4, 0, 0, &[0x06, 0, 1, 0, 2])),
        ]));
        let mut buf = [0; MAX_PDU];
        let r = udp.recv(&mut buf).await.unwrap();
        assert_eq!((r.unit, &buf[..r.len]), (0xFF, &[0x06, 0, 1, 0, 2][..]));
        udp.link.send = Err(LinkError::Lost("нет буфера"));
        assert_eq!(udp.reply(&[0x06, 0, 1, 0, 2]).await, Ok(()));
        udp.link.send = Ok(());
        udp.reply(&[0x06, 0, 1, 0, 2]).await.unwrap();
        assert_eq!(udp.link.sent, [frame(4, 0, 0, &[0x06, 0, 1, 0, 2])]);
        assert!(matches!(
            udp.recv(&mut buf).await,
            Err(ServeError::Io(LinkError::Down(_)))
        ));
    }

    #[tokio::test]
    async fn client_skips_stale_transactions() {
        let mut udp = Udp::new(Datagrams::with([
            Ok(frame(0, 0, 1, &[0x03, 2, 0, 1])),
            Ok(frame(1, 0, 1, &[0x03, 2, 0, 2])),
        ]));
        let mut out = [0; MAX_PDU];
        assert_eq!(udp.exchange(1, &[0x03, 0, 0, 0, 1], &mut out).await, Ok(4));
        assert_eq!(out[..4], [0x03, 2, 0, 2]);
        assert_eq!(udp.link.sent, [frame(1, 0, 1, &[0x03, 0, 0, 0, 1])]);
    }

    #[tokio::test]
    async fn client_rejects_foreign_unit_and_gives_up_on_losses() {
        let mut udp = Udp::new(Datagrams::with([Ok(frame(1, 0, 2, &[0x03, 2, 0, 1]))]));
        let mut out = [0; MAX_PDU];
        assert_eq!(
            udp.exchange(1, &[0x03, 0, 0, 0, 1], &mut out).await,
            Err(MasterError::Unexpected)
        );

        let losses = (0..MAX_LOST_IN_A_ROW).map(|_| Err(LinkError::Lost("помеха")));
        let mut udp = Udp::new(Datagrams::with(losses.chain([Ok(frame(
            1,
            0,
            1,
            &[0x03, 2, 0, 1],
        ))])));
        assert_eq!(
            udp.exchange(1, &[0x03, 0, 0, 0, 1], &mut out).await,
            Err(MasterError::Io(LinkError::Lost("помеха")))
        );
        assert_eq!(
            udp.link.incoming.len(),
            1,
            "приём не остановился на пределе"
        );
    }

    proptest::proptest! {
        /// Какая бы датаграмма ни пришла — ни slave, ни master не паникуют.
        #[test]
        fn never_panics(datagram in proptest::collection::vec(proptest::num::u8::ANY, 0..300)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let mut buf = [0; MAX_PDU];
                let mut udp = Udp::new(Datagrams::with([Ok(datagram.clone())]));
                let _ignored = udp.recv(&mut buf).await;
                let mut udp = Udp::new(Datagrams::with([Ok(datagram)]));
                let _ignored = udp.exchange(1, &[0x03, 0, 0, 0, 1], &mut buf).await;
            });
        }
    }
}
