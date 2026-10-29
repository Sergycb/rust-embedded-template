//! Modbus UDP: заголовок MBAP и PDU, по кадру в датаграмме.

use ports::{FrameLink, LinkError};

// {%- if modbus_master == "true" %}
use ports::{MAX_PDU, MasterError, ModbusClient};

// {%- endif %}
use super::mbap::{self, HEADER, Header, MAX_ADU};

/// Сколько байт нужно буферу канала под [`Udp`]: самая длинная датаграмма
/// Modbus — заголовок MBAP и PDU.
pub const DATAGRAM: usize = MAX_ADU;

/// Сколько потерянных датаграмм подряд терпит приём, прежде чем объявить
/// канал отказавшим: серия потерь — уже не помеха, и узлу о ней стоит знать.
/// Переждать отказ — дело канала под транспортом (`ports::FrameLink`).
const MAX_LOST_IN_A_ROW: u8 = 32;

/// Modbus UDP поверх датаграмм `link` (UDP-сокет `embassy-net` за
/// [`FrameLink`]). Датаграмма — принятая и собираемая к отправке — лежит в
/// буфере канала: ему нужно не меньше [`DATAGRAM`] байт.
///
/// Отвечать slave'у некуда, кроме как отправителю последнего запроса: этот
/// адрес помнит реализация `link`, порт его не передаёт.
///
/// Битая датаграмма (короче заголовка, длина в MBAP не совпала с
/// датаграммой, чужой протокол) пропускается.
// {%- if modbus_master == "true" %}
/// Master пропускает и ответ с чужим номером транзакции.
// {%- endif %}
// {%- if modbus_master == "true" %}
/// Потерянный ответ ([`LinkError::Lost`]) master переживает повтором запроса.
// {%- endif %}
/// Для slave'а — сам канал [`FrameLink`] с кадром «адрес и PDU» в той же
/// датаграмме, за заголовком: любой запрос приходит как адресованный самому
/// устройству, 0xFF (см. [модуль](super)), а ответ несёт адрес и номер
/// транзакции запроса.
#[derive(Debug)]
pub struct Udp<L> {
    link: L,
    /// Номер транзакции последнего принятого запроса — им подписан ответ.
    // {%- if modbus_master == "true" %}
    /// У master — последнего отправленного: по нему ищется ответ.
    // {%- endif %}
    tid: u16,
    /// Адрес из последнего принятого запроса — slave повторяет его в ответе.
    unit: u8,
}

impl<L: FrameLink> Udp<L> {
    /// Транспорт поверх датаграмм `link`.
    ///
    /// # Panics
    ///
    /// Если буфер канала меньше [`DATAGRAM`]: самая длинная датаграмма Modbus
    /// в него не влезет.
    pub fn new(mut link: L) -> Self {
        let len = link.buffer().len();
        assert!(len >= DATAGRAM, "буфер канала {len} < {DATAGRAM}");
        Self {
            link,
            tid: 0,
            unit: 0,
        }
    }

    /// Ждёт целую датаграмму Modbus в `buf`; возвращает заголовок и длину.
    async fn datagram(&mut self) -> Result<(Header, usize), LinkError<L::Error>> {
        let mut lost = 0_u8;
        loop {
            match self.link.recv().await {
                Ok(len) if len >= HEADER => {
                    let header = Header::parse(self.link.buffer());
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

    // {%- if modbus_master == "true" %}
    /// Собирает датаграмму и отправляет.
    async fn send_pdu(
        &mut self,
        tid: u16,
        unit: u8,
        pdu: &[u8],
    ) -> Result<(), LinkError<L::Error>> {
        let buf = self.link.buffer();
        buf[..HEADER].copy_from_slice(&mbap::head(tid, unit, pdu.len()));
        buf[HEADER..HEADER + pdu.len()].copy_from_slice(pdu);
        self.link.send(HEADER + pdu.len()).await
    }
    // {%- endif %}
}

impl<L: FrameLink> FrameLink for Udp<L> {
    type Error = L::Error;

    async fn recv(&mut self) -> Result<usize, LinkError<L::Error>> {
        let (header, len) = self.datagram().await?;
        self.tid = header.tid;
        self.unit = header.unit;
        self.link.buffer()[HEADER - 1] = 0xFF;
        Ok(len - (HEADER - 1))
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.link.buffer()[HEADER - 1..]
    }

    /// Ответ — с номером транзакции и адресом запроса: заголовок пишется
    /// перед кадром в той же датаграмме.
    async fn send(&mut self, len: usize) -> Result<(), LinkError<L::Error>> {
        let pdu = len.checked_sub(1).expect("кадр ответа — адрес и PDU");
        let head = mbap::head(self.tid, self.unit, pdu);
        self.link.buffer()[..HEADER].copy_from_slice(&head);
        self.link.send(HEADER + pdu).await
    }
}

// {%- if modbus_master == "true" %}
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
        self.send_pdu(self.tid, unit, request)
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
            let pdu = &self.link.buffer()[HEADER..len];
            response[..pdu.len()].copy_from_slice(pdu);
            return Ok(pdu.len());
        }
    }

    async fn broadcast(&mut self, request: &[u8]) -> Result<(), MasterError<Self::Error>> {
        self.tid = self.tid.wrapping_add(1);
        self.send_pdu(self.tid, 0, request)
            .await
            .map_err(MasterError::Io)
    }
}
// {%- endif %}

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
        let len = udp.recv().await.unwrap();
        assert_eq!(&udp.buffer()[..len], &[0xFF, 0x06, 0, 1, 0, 2]);
        udp.link.send = Err(LinkError::Lost("нет буфера"));
        assert_eq!(udp.send(len).await, Err(LinkError::Lost("нет буфера")));
        udp.link.send = Ok(());
        udp.send(len).await.unwrap();
        assert_eq!(udp.link.sent, [frame(4, 0, 0, &[0x06, 0, 1, 0, 2])]);
        assert!(matches!(udp.recv().await, Err(LinkError::Down(_))));
    }

    // {%- if modbus_master == "true" %}
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
    // {%- endif %}

    proptest::proptest! {
        /// Какая бы датаграмма ни пришла — транспорт не паникует.
        #[test]
        fn never_panics(datagram in proptest::collection::vec(proptest::num::u8::ANY, 0..300)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let mut udp = Udp::new(Datagrams::with([Ok(datagram.clone())]));
                let _ignored = udp.recv().await;
                // {%- if modbus_master == "true" %}
                let mut buf = [0; MAX_PDU];
                let mut udp = Udp::new(Datagrams::with([Ok(datagram)]));
                let _ignored = udp.exchange(1, &[0x03, 0, 0, 0, 1], &mut buf).await;
                // {%- endif %}
            });
        }
    }
}
