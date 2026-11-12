//! Каналы Modbus: протокол поверх транспорта.
//!
//! Кадр собирается по правилам протокола — `rtu` (адрес, PDU, CRC) или
//! `mbap` (заголовок MBAP, общий для TCP и UDP), — а байты приносит
//! транспорт: поток `embedded_io_async::{Read, Write}` или канал датаграмм
//! [`ports::FrameLink`]. Сам транспорт строится вне `modbus`: `net::Server`,
//! `net::UdpLink` и рычаги платы.
//!
//! Для slave'а каждый канал — [`ports::FrameLink`] с кадром «адрес и PDU»:
//! кадр разбирает обработчик `domain::modbus::Slave`, а обслуживает канал
//! узел `domain::modbus`.
// {%- if modbus_master == "true" %}
//! Они же реализуют порт master'а [`ports::ModbusClient`].
// {%- endif %}

use ports::{FrameLink, LinkError};

// {%- if modbus_mbap == "true" %}
use super::mbap;
// {%- endif %}
// {%- if modbus_rtu == "true" %}
use super::rtu;
// {%- endif %}
// {%- if modbus_tcp == "true" %}
use crate::net::{Error as NetError, Server};
// {%- endif %}
// {%- if modbus_stream == "true" %}
use embedded_io_async::{Error, ErrorKind, Read, ReadExactError, Write};
// {%- endif %}
// {%- if modbus_master == "true" %}
use ports::MAX_PDU;
// {%- endif %}

// {%- if modbus_rtu == "true" %}
// {%- if modbus_master == "true" %}
/// Сколько байт слива хватит, чтобы убрать опоздавший ответ и хвост битого
/// кадра, — не больше двух кадров. Предел нужен, чтобы линия, на которой
/// приём не кончается (шум, второй master), не держала запрос вечно.
const MAX_DRAIN: usize = 2 * rtu::MAX_ADU;
// {%- endif %}
// {%- endif %}
// {%- if modbus_udp == "true" %}
/// Сколько байт нужно буферу канала под [`Udp`]: самая длинная датаграмма
/// Modbus — заголовок MBAP и PDU.
pub const DATAGRAM: usize = mbap::MAX_ADU;

/// Сколько потерянных датаграмм подряд приём пропускает молча, прежде чем
/// вернуть потерю ([`LinkError::Lost`] с последней ошибкой): серия потерь —
/// уже не помеха, и узлу о ней стоит знать.
/// Переждать отказ — дело канала под транспортом (`ports::FrameLink`).
const MAX_LOST_IN_A_ROW: u8 = 32;
// {%- endif %}

// {%- if modbus_stream == "true" %}
/// Почему поток Modbus (RTU, TCP) не принял или не отправил кадр.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum FrameError<E> {
    /// Отказ потока.
    Io(E),
    /// Поток кончился: соединение закрыто. Потеря, а не отказ канала: над
    /// TCP-сервером (`net::Server`) следующий `recv` примет нового клиента.
    Eof,
    /// Длина в заголовке MBAP вне допустимой: граница кадра в потоке потеряна.
    /// Slave обрывает соединение (`net::Server`), следующий `recv` примет
    /// нового клиента.
    Framing,
}
// {%- endif %}

// {%- if modbus_rtu == "true" %}
/// Modbus RTU поверх потока байтов `io`.
// {%- if has_uart == "true" %}
/// На UART это `BufferedUart` с DE для RS-485.
// {%- endif %}
///
/// Slave ([`FrameLink`], кадр «адрес и PDU») не принимает кадр с неверной CRC.
/// Длину кадра он выводит для всех открытых функций спецификации, в том
/// числе тех, что обработчик не исполняет и на которые ответит Illegal
/// Function (FC07, 08, 11, 12, 17, 20–24). Кадр пользовательской функции
/// (65–72, 100–110), MEI, кроме 43/14 (Read Device Identification), и
/// FC08/00 с данными длиннее двух байт пропускается: длину его кадра не
/// вывести, а отвечать, не проверив CRC, нельзя — master получит таймаут, а
/// не Illegal Function. Кадры на
/// зарезервированные адреса 248–255 пропускаются. Начатый кадр, после
/// которого линия молчит дольше 20 мс (`FRAME_GAP`), slave выбрасывает:
/// иначе помеха, сделавшая из функции FC16, заставила бы его ждать до 256
/// байт, копя следующие запросы.
///
// {%- if modbus_master == "true" %}
/// Master ([`ports::ModbusClient`]) перед каждым запросом сливает всё, что
/// уже лежит во входном буфере (`ReadReady` — его реализуют `BufferedUart` и
/// `RingBufferedUartRx` из `embassy-stm32`): брошенный на таймауте запрос
/// оставляет в потоке опоздавший ответ, неразобранный кадр — свой хвост.
/// Не закрыт один случай: ответ, который ещё в пути, когда уходит следующий
/// запрос, — тот будет прочитан как ответ на новый. Таймаут поэтому стоит
/// брать с запасом на время ответа самого медленного slave'а.
///
// {%- endif %}
/// На общей шине RS-485 slave видит и ответы других slave'ов, и чужой ответ
/// может на время сбить его с границы кадра, пока CRC не выровняет поток.
#[derive(Debug)]
pub struct Rtu<T> {
    io: T,
    /// Принятые, но ещё не разобранные байты — `rx[..have]`.
    rx: [u8; rtu::MAX_ADU],
    have: usize,
    /// Буфер канала: принятый кадр «адрес и PDU», затем ответ — и место под
    /// CRC за ним. Отдельно от `rx`: там за кадром могут лежать уже принятые
    /// байты следующего, и ответ поверх них их бы затёр.
    frame: [u8; rtu::MAX_ADU],
}

impl<T> Rtu<T> {
    /// Канал поверх потока `io`.
    pub const fn new(io: T) -> Self {
        Self {
            io,
            rx: [0; rtu::MAX_ADU],
            have: 0,
            frame: [0; rtu::MAX_ADU],
        }
    }
}

impl<T: Read + Write> FrameLink for Rtu<T> {
    type Error = FrameError<T::Error>;

    async fn recv(&mut self) -> Result<usize, LinkError<Self::Error>> {
        loop {
            let len = self.next_request().await.map_err(read_error)?;
            let body = len - 2;
            let reserved = self.rx[0] > 247;
            if !reserved {
                self.frame[..body].copy_from_slice(&self.rx[..body]);
            }
            self.consume(len);
            if !reserved {
                return Ok(body);
            }
        }
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.frame[..rtu::MAX_ADU - 2]
    }

    /// Ответ — с адресом, который обработчик оставил в начале кадра.
    async fn send(&mut self, len: usize) -> Result<(), LinkError<Self::Error>> {
        let total = rtu::seal(&mut self.frame, len);
        self.io
            .write_all(&self.frame[..total])
            .await
            .map_err(io_error)?;
        self.io.flush().await.map_err(io_error)
    }
}

// {%- if modbus_master == "true" %}
impl<T: Read + embedded_io_async::ReadReady + Write> ports::ModbusClient for Rtu<T> {
    type Error = T::Error;

    async fn exchange(
        &mut self,
        unit: u8,
        request: &[u8],
        response: &mut [u8],
    ) -> Result<usize, ports::MasterError<T::Error>> {
        assert!(
            (1..=247).contains(&unit),
            "запрос на адрес {unit}: у slave'а RTU адрес 1..=247"
        );
        assert!(
            response.len() >= MAX_PDU,
            "буфер PDU {} < {MAX_PDU}",
            response.len()
        );
        self.send_request(unit, request).await?;
        let len = self.receive().await?;
        if self.rx[0] != unit {
            return Err(ports::MasterError::Unexpected);
        }
        let pdu = &self.rx[1..len - 2];
        response[..pdu.len()].copy_from_slice(pdu);
        Ok(pdu.len())
    }

    async fn broadcast(&mut self, request: &[u8]) -> Result<(), ports::MasterError<T::Error>> {
        self.send_request(0, request).await
    }
}
// {%- endif %}

impl<T: Read> Rtu<T> {
    /// Ждёт кадр запроса с верной CRC в начале `rx`; возвращает его длину.
    ///
    /// Читает ровно столько, сколько не хватает до кадра, — ни байтом
    /// больше, иначе начало следующего кадра пришлось бы хранить где-то ещё.
    async fn next_request(&mut self) -> Result<usize, ReadExactError<T::Error>> {
        loop {
            if !self.fill(2).await? {
                continue;
            }
            let Some(header) = rtu::request_header(self.rx[1]) else {
                self.consume(1);
                continue;
            };
            if !self.fill(header).await? {
                continue;
            }
            let len = rtu::request_len(&self.rx[..header]);
            if len <= rtu::MAX_ADU {
                if !self.fill(len).await? {
                    continue;
                }
                if rtu::crc_ok(&self.rx[..len]) {
                    return Ok(len);
                }
            }
            self.consume(1);
        }
    }

    /// Дочитывает `rx` до `want` байт; `false` — линия смолкла посреди
    /// начатого кадра дольше [`rtu::FRAME_GAP`], и начатое выброшено: разбор
    /// начинается заново.
    ///
    /// Пустой буфер ждёт без срока — тишина между запросами обычна. Каждый
    /// прочитанный кусок сразу учтён в `have`, а брошенное по сроку или
    /// снаружи чтение ничего не забирает из потока: следующий вызов
    /// продолжит с того же места.
    async fn fill(&mut self, want: usize) -> Result<bool, ReadExactError<T::Error>> {
        while self.have < want {
            let read = self.io.read(&mut self.rx[self.have..want]);
            let read = if self.have == 0 {
                read.await
            } else if let Ok(read) = embassy_time::with_timeout(rtu::FRAME_GAP, read).await {
                read
            } else {
                self.have = 0;
                return Ok(false);
            };
            match read {
                Ok(0) => return Err(ReadExactError::UnexpectedEof),
                Ok(n) => self.have += n,
                Err(e) => return Err(ReadExactError::Other(e)),
            }
        }
        Ok(true)
    }

    /// Убирает `n` байт из начала принятого.
    fn consume(&mut self, n: usize) {
        self.rx.copy_within(n..self.have, 0);
        self.have -= n;
    }

    // {%- if modbus_master == "true" %}
    /// Принимает один кадр ответа в начало `rx`; возвращает его длину.
    async fn receive(&mut self) -> Result<usize, ports::MasterError<T::Error>> {
        let rx = &mut self.rx;
        self.io
            .read_exact(&mut rx[..2])
            .await
            .map_err(master_error)?;
        let header = rtu::response_header(rx[1]).ok_or(ports::MasterError::Unexpected)?;
        self.io
            .read_exact(&mut rx[2..header])
            .await
            .map_err(master_error)?;
        let len = rtu::response_len(&rx[..header]);
        if len > rtu::MAX_ADU {
            return Err(ports::MasterError::Unexpected);
        }
        self.io
            .read_exact(&mut rx[header..len])
            .await
            .map_err(master_error)?;
        if rtu::crc_ok(&rx[..len]) {
            Ok(len)
        } else {
            Err(ports::MasterError::Crc)
        }
    }
    // {%- endif %}
}

// {%- if modbus_master == "true" %}
impl<T: Read + embedded_io_async::ReadReady + Write> Rtu<T> {
    /// Сливает входной буфер и отправляет кадр запроса.
    async fn send_request(
        &mut self,
        unit: u8,
        request: &[u8],
    ) -> Result<(), ports::MasterError<T::Error>> {
        self.drain().await?;
        let mut tx = [0; rtu::MAX_ADU];
        let total = rtu::frame(&mut tx, unit, request);
        self.io
            .write_all(&tx[..total])
            .await
            .map_err(ports::MasterError::Io)?;
        self.io.flush().await.map_err(ports::MasterError::Io)
    }

    /// Выбрасывает то, что уже принято: опоздавший ответ на брошенный запрос
    /// или хвост неразобранного кадра. Иначе ответ на этот запрос читался бы
    /// со сдвигом, а опоздавший ответ на чтение был бы принят за свой — с
    /// верной CRC и чужими данными.
    async fn drain(&mut self) -> Result<(), ports::MasterError<T::Error>> {
        let mut scratch = [0_u8; 32];
        let mut drained = 0;
        while drained < MAX_DRAIN && self.io.read_ready().map_err(ports::MasterError::Io)? {
            let n = self
                .io
                .read(&mut scratch)
                .await
                .map_err(ports::MasterError::Io)?;
            if n == 0 {
                // Конец потока: дальше сливать нечего, а заметит его приём ответа.
                break;
            }
            drained += n;
        }
        Ok(())
    }
}
// {%- endif %}
// {%- endif %}

// {%- if modbus_tcp == "true" %}
/// Modbus TCP: заголовок MBAP и PDU поверх потока байтов `io`.
///
/// Принятое копится во внутреннем буфере, а не читается `read_exact` в
/// локальный: брошенный на таймауте обмен оставляет недочитанный кадр там,
/// и следующий дочитывает его и пропускает по номеру транзакции — граница
/// кадра в потоке не теряется. Отправка так не защищена: брошенная посреди
/// записи оставляет на линии полкадра, после чего соединение стоит
/// переоткрыть.
///
/// Для slave'а — канал [`FrameLink`] над [`Server`] с кадром «адрес и PDU» прямо в приёмном
/// буфере, за заголовком: любой запрос приходит как адресованный самому
/// устройству, 0xFF (см. [модуль](super)), а ответ несёт адрес и номер
/// транзакции запроса.
// {%- if modbus_master == "true" %}
/// Master ждёт ответ с тем же номером транзакции, что у запроса, и
/// устройство, которое его не повторяет, так и не ответит.
// {%- endif %}
///
/// Соединение slave'а — забота [`Server`]: закрытое клиентом — потеря
/// кадра ([`FrameError::Eof`]), а не отказ: узел обслуживает дальше, а
/// следующее чтение `Server` примет новое соединение. Длина в
/// заголовке вне допустимой — граница кадра потеряна, и slave обрывает
/// соединение сам (`Server::hang_up`): клиент переподключится и начнёт с
/// чистого потока. Кадр без PDU (только адрес) пропускается — его граница известна.
#[derive(Debug)]
pub struct Tcp<T> {
    io: T,
    /// Принятые байты текущего кадра — `rx[..have]`; для slave'а с
    /// адреса (`rx[mbap::HEADER - 1..]`) это и буфер канала.
    rx: [u8; mbap::MAX_ADU],
    have: usize,
    /// Номер транзакции последнего принятого запроса — им подписан ответ.
    // {%- if modbus_master == "true" %}
    /// У master — последнего отправленного: по нему ищется ответ.
    // {%- endif %}
    tid: u16,
    /// Адрес из последнего принятого запроса — slave повторяет его в ответе.
    unit: u8,
}

impl<T> Tcp<T> {
    /// Канал поверх потока `io`.
    pub const fn new(io: T) -> Self {
        Self {
            io,
            rx: [0; mbap::MAX_ADU],
            have: 0,
            tid: 0,
            unit: 0,
        }
    }
}

/// Почему не принят кадр.
enum Recv<E> {
    Read(ReadExactError<E>),
    /// Длина в заголовке вне допустимой: граница кадра потеряна.
    Framing,
}

impl<E> From<ReadExactError<E>> for Recv<E> {
    fn from(e: ReadExactError<E>) -> Self {
        Self::Read(e)
    }
}

impl<T: Read + Write> Tcp<T> {
    /// Принимает кадр целиком в начало `rx`; возвращает заголовок и длину
    /// кадра.
    async fn frame(&mut self) -> Result<(rusty_modbus_types::MbapHeader, usize), Recv<T::Error>> {
        loop {
            self.fill(mbap::HEADER).await?;
            let header = mbap::parse(&self.rx);
            if let Some(total) = mbap::total(header) {
                self.fill(total).await?;
                self.have = 0;
                return Ok((header, total));
            }
            self.have = 0;
            if !mbap::is_empty(header) {
                return Err(Recv::Framing);
            }
        }
    }

    /// Брошенное чтение (таймаут) недочитанный кадр сохраняет — следующий
    /// обмен его дочитает. Оборванное (конец потока, ошибка) — сбрасывает:
    /// поток над TCP-сервером продолжится уже другим соединением, и хвост
    /// прошлого склеился бы с его первым кадром.
    async fn fill(&mut self, want: usize) -> Result<(), ReadExactError<T::Error>> {
        let result = fill(&mut self.io, &mut self.rx, &mut self.have, want).await;
        if result.is_err() {
            self.have = 0;
        }
        result
    }

    // {%- if modbus_master == "true" %}
    async fn send_pdu(&mut self, tid: u16, unit: u8, pdu: &[u8]) -> Result<(), T::Error> {
        self.io.write_all(&mbap::head(tid, unit, pdu.len())).await?;
        self.io.write_all(pdu).await?;
        self.io.flush().await
    }
    // {%- endif %}
}

impl FrameLink for Tcp<Server> {
    type Error = FrameError<NetError>;

    async fn recv(&mut self) -> Result<usize, LinkError<Self::Error>> {
        loop {
            let (header, total) = match self.frame().await {
                Ok(frame) => frame,
                Err(Recv::Read(e)) => return Err(read_error(e)),
                Err(Recv::Framing) => {
                    // Границу кадра в этом соединении уже не найти: всё, что
                    // придёт по нему дальше, читалось бы как заголовки.
                    self.io.hang_up().await;
                    return Err(LinkError::Lost(FrameError::Framing));
                }
            };
            if header.protocol_id.get() != 0 {
                // Чужой протокол поверх MBAP: спецификация велит его пропустить.
                continue;
            }
            self.tid = header.transaction_id.get();
            self.unit = header.unit_id;
            self.rx[mbap::HEADER - 1] = 0xFF;
            return Ok(total - (mbap::HEADER - 1));
        }
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.rx[mbap::HEADER - 1..]
    }

    /// Ответ — с номером транзакции и адресом запроса: заголовок пишется
    /// перед кадром в том же буфере.
    async fn send(&mut self, len: usize) -> Result<(), LinkError<Self::Error>> {
        let pdu = len.checked_sub(1).expect("кадр ответа — адрес и PDU");
        self.rx[..mbap::HEADER].copy_from_slice(&mbap::head(self.tid, self.unit, pdu));
        self.io
            .write_all(&self.rx[..mbap::HEADER + pdu])
            .await
            .map_err(io_error)?;
        self.io.flush().await.map_err(io_error)
    }
}

// {%- if modbus_master == "true" %}
impl<T: Read + Write> ports::ModbusClient for Tcp<T> {
    type Error = T::Error;

    async fn exchange(
        &mut self,
        unit: u8,
        request: &[u8],
        response: &mut [u8],
    ) -> Result<usize, ports::MasterError<T::Error>> {
        assert!(
            response.len() >= MAX_PDU,
            "буфер PDU {} < {MAX_PDU}",
            response.len()
        );
        self.tid = self.tid.wrapping_add(1);
        self.send_pdu(self.tid, unit, request)
            .await
            .map_err(ports::MasterError::Io)?;
        loop {
            let (header, total) = self.frame().await.map_err(|e| match e {
                Recv::Read(e) => master_error(e),
                Recv::Framing => ports::MasterError::Unexpected,
            })?;
            if header.protocol_id.get() != 0 || header.transaction_id.get() != self.tid {
                // Опоздавший ответ на брошенный запрос или чужой протокол.
                continue;
            }
            if header.unit_id != unit {
                return Err(ports::MasterError::Unexpected);
            }
            let pdu = &self.rx[mbap::HEADER..total];
            response[..pdu.len()].copy_from_slice(pdu);
            return Ok(pdu.len());
        }
    }

    async fn broadcast(&mut self, request: &[u8]) -> Result<(), ports::MasterError<T::Error>> {
        self.tid = self.tid.wrapping_add(1);
        self.send_pdu(self.tid, 0, request)
            .await
            .map_err(ports::MasterError::Io)
    }
}
// {%- endif %}
// {%- endif %}

// {%- if modbus_udp == "true" %}
/// Modbus UDP: заголовок MBAP и PDU, по кадру в датаграмме `link`
/// (UDP-сокет `embassy-net` за [`FrameLink`]). Датаграмма — принятая и
/// собираемая к отправке — лежит в буфере канала: ему нужно не меньше
/// [`DATAGRAM`] байт.
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
/// Ответ, который не пришёл, `exchange` ждёт сколько угодно и запрос не
/// повторяет: ограничить ожидание (`with_timeout`) и повторить запрос — дело
/// вызывающего.
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
    /// Канал поверх датаграмм `link`.
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
    async fn datagram(
        &mut self,
    ) -> Result<(rusty_modbus_types::MbapHeader, usize), LinkError<L::Error>> {
        let mut lost = 0_u8;
        loop {
            match self.link.recv().await {
                Ok(len) if len >= mbap::HEADER => {
                    let header = mbap::parse(self.link.buffer());
                    if header.protocol_id.get() == 0 && mbap::total(header) == Some(len) {
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
        buf[..mbap::HEADER].copy_from_slice(&mbap::head(tid, unit, pdu.len()));
        buf[mbap::HEADER..mbap::HEADER + pdu.len()].copy_from_slice(pdu);
        self.link.send(mbap::HEADER + pdu.len()).await
    }
    // {%- endif %}
}

impl<L: FrameLink> FrameLink for Udp<L> {
    type Error = L::Error;

    async fn recv(&mut self) -> Result<usize, LinkError<L::Error>> {
        let (header, len) = self.datagram().await?;
        self.tid = header.transaction_id.get();
        self.unit = header.unit_id;
        self.link.buffer()[mbap::HEADER - 1] = 0xFF;
        Ok(len - (mbap::HEADER - 1))
    }

    fn buffer(&mut self) -> &mut [u8] {
        &mut self.link.buffer()[mbap::HEADER - 1..]
    }

    /// Ответ — с номером транзакции и адресом запроса: заголовок пишется
    /// перед кадром в той же датаграмме.
    async fn send(&mut self, len: usize) -> Result<(), LinkError<L::Error>> {
        let pdu = len.checked_sub(1).expect("кадр ответа — адрес и PDU");
        let head = mbap::head(self.tid, self.unit, pdu);
        self.link.buffer()[..mbap::HEADER].copy_from_slice(&head);
        self.link.send(mbap::HEADER + pdu).await
    }
}

// {%- if modbus_master == "true" %}
impl<L: FrameLink> ports::ModbusClient for Udp<L> {
    type Error = LinkError<L::Error>;

    async fn exchange(
        &mut self,
        unit: u8,
        request: &[u8],
        response: &mut [u8],
    ) -> Result<usize, ports::MasterError<Self::Error>> {
        assert!(
            response.len() >= MAX_PDU,
            "буфер PDU {} < {MAX_PDU}",
            response.len()
        );
        self.tid = self.tid.wrapping_add(1);
        self.send_pdu(self.tid, unit, request)
            .await
            .map_err(ports::MasterError::Io)?;
        loop {
            let (header, len) = self.datagram().await.map_err(ports::MasterError::Io)?;
            if header.transaction_id.get() != self.tid {
                // Опоздавший ответ на брошенный запрос.
                continue;
            }
            if header.unit_id != unit {
                return Err(ports::MasterError::Unexpected);
            }
            let pdu = &self.link.buffer()[mbap::HEADER..len];
            response[..pdu.len()].copy_from_slice(pdu);
            return Ok(pdu.len());
        }
    }

    async fn broadcast(&mut self, request: &[u8]) -> Result<(), ports::MasterError<Self::Error>> {
        self.tid = self.tid.wrapping_add(1);
        self.send_pdu(self.tid, 0, request)
            .await
            .map_err(ports::MasterError::Io)
    }
}
// {%- endif %}
// {%- endif %}

// {%- if modbus_stream == "true" %}
/// Отказ потока как исход канала: разорванное соединение — отказ, шум на
/// линии (переполнение, чётность; `embassy-stm32` отдаёт их как
/// `ErrorKind::Other`) — потеря кадра.
fn io_error<E: Error>(e: E) -> LinkError<FrameError<E>> {
    if matches!(
        e.kind(),
        ErrorKind::ConnectionReset
            | ErrorKind::ConnectionAborted
            | ErrorKind::ConnectionRefused
            | ErrorKind::NotConnected
            | ErrorKind::BrokenPipe
    ) {
        LinkError::Down(FrameError::Io(e))
    } else {
        LinkError::Lost(FrameError::Io(e))
    }
}

fn read_error<E: Error>(e: ReadExactError<E>) -> LinkError<FrameError<E>> {
    match e {
        ReadExactError::UnexpectedEof => LinkError::Lost(FrameError::Eof),
        ReadExactError::Other(e) => io_error(e),
    }
}

// {%- if modbus_master == "true" %}
fn master_error<E>(e: ReadExactError<E>) -> ports::MasterError<E> {
    match e {
        ReadExactError::UnexpectedEof => ports::MasterError::Eof,
        ReadExactError::Other(e) => ports::MasterError::Io(e),
    }
}
// {%- endif %}
// {%- endif %}

// {%- if modbus_tcp == "true" %}
/// Дочитывает `rx` из `io` до `want` байт. Каждый прочитанный кусок сразу
/// учтён в `have`, поэтому брошенное чтение ничего не теряет: следующий
/// вызов продолжит с того же места.
async fn fill<T: embedded_io_async::Read>(
    io: &mut T,
    rx: &mut [u8],
    have: &mut usize,
    want: usize,
) -> Result<(), ReadExactError<T::Error>> {
    while *have < want {
        match io.read(&mut rx[*have..want]).await {
            Ok(0) => return Err(ReadExactError::UnexpectedEof),
            Ok(n) => *have += n,
            Err(e) => return Err(ReadExactError::Other(e)),
        }
    }
    Ok(())
}
// {%- endif %}

#[cfg(test)]
mod tests {
    // {%- if modbus_master == "true" %}
    use ports::ModbusClient;
    // {%- endif %}
    // {%- if modbus_master == "true" %}
    // {%- if modbus_rtu == "true" or modbus_tcp == "true" %}
    use core::convert::Infallible;
    // {%- endif %}
    // {%- endif %}
    // {%- if modbus_rtu == "true" %}
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};
    // {%- endif %}
    // {%- if modbus_rtu == "true" %}
    use embassy_time::{Duration, MockDriver};
    // {%- endif %}
    // {%- if modbus_tcp == "true" %}
    use adapters_hw::net::fake::cable::{DEVICE, leak, pair};
    use embassy_net::Stack;
    use embassy_net::tcp::TcpSocket;
    // {%- endif %}

    use super::*;
    // {%- if modbus_stream == "true" %}
    use crate::modbus::fake::Duplex;
    // {%- endif %}
    // {%- if modbus_mbap == "true" %}
    use crate::modbus::fake::mbap_frame;
    // {%- endif %}
    // {%- if modbus_rtu == "true" %}
    use crate::modbus::fake::rtu_frame;
    // {%- endif %}
    // {%- if modbus_udp == "true" %}
    use crate::modbus::fake::Datagrams;
    // {%- endif %}
    // {%- if modbus_tcp == "true" %}
    use crate::net::fake::{PORT, with_network};
    // {%- endif %}

    // {%- if modbus_rtu == "true" %}
    /// Все запросы из `input` до конца потока: адрес и PDU.
    async fn requests(input: &[u8]) -> Vec<(u8, Vec<u8>)> {
        let mut rtu = Rtu::new(Duplex::new(input));
        let mut got = Vec::new();
        loop {
            match rtu.recv().await {
                Ok(len) => {
                    let frame = &rtu.buffer()[..len];
                    got.push((frame[0], frame[1..].to_vec()));
                }
                Err(e) => {
                    assert_eq!(e, LinkError::Lost(FrameError::Eof));
                    return got;
                }
            }
        }
    }

    #[tokio::test]
    async fn server_receives_and_replies() {
        let mut rtu = Rtu::new(Duplex::new(&rtu_frame(7, &[0x03, 0, 1, 0, 1])));
        let len = rtu.recv().await.unwrap();
        assert_eq!(&rtu.buffer()[..len], &[7, 0x03, 0, 1, 0, 1]);
        rtu.buffer()[1..5].copy_from_slice(&[0x03, 2, 0, 5]);
        rtu.send(5).await.unwrap();
        assert_eq!(rtu.io.tx, rtu_frame(7, &[0x03, 2, 0, 5]));
    }

    #[tokio::test]
    async fn server_receives_read_device_identification() {
        let input = [
            rtu_frame(1, &[0x2B, 0x0E, 0x01, 0x00]),
            rtu_frame(1, &[0x03, 0, 0, 0, 1]),
        ]
        .concat();
        assert_eq!(
            requests(&input).await,
            [
                (1, vec![0x2B, 0x0E, 0x01, 0x00]),
                (1, vec![0x03, 0, 0, 0, 1])
            ]
        );
    }

    /// Кадр функции, которой slave не исполняет, канал всё равно находит и
    /// отдаёт обработчику (тот ответит Illegal Function), а следующий за ним
    /// запрос читается с верной границы.
    #[tokio::test]
    async fn server_frames_functions_it_does_not_execute() {
        let pdus: [&[u8]; 10] = [
            &[0x07],
            &[0x08, 0x00, 0x00, 0xA5, 0x37],
            &[0x0B],
            &[0x0C],
            &[0x11],
            &[0x14, 0x07, 0x06, 0x00, 0x04, 0x00, 0x01, 0x00, 0x02],
            &[
                0x15, 0x09, 0x06, 0x00, 0x04, 0x00, 0x07, 0x00, 0x01, 0x06, 0xAF,
            ],
            &[0x16, 0x00, 0x04, 0x00, 0xF2, 0x00, 0x25],
            &[0x17, 0, 3, 0, 6, 0, 14, 0, 3, 6, 0, 0xFF, 0, 0xFF, 0, 0xFF],
            &[0x18, 0x04, 0xDE],
        ];
        let read = [0x03, 0, 0, 0, 1];
        for pdu in pdus {
            let input = [rtu_frame(1, pdu), rtu_frame(1, &read)].concat();
            assert_eq!(
                requests(&input).await,
                [(1, pdu.to_vec()), (1, read.to_vec())],
                "FC{:02X}",
                pdu[0]
            );
        }
    }

    /// Помеха сделала из функции запроса FC16: по «счётчику байт» кадр ждал
    /// бы 249 байт, копя следующие запросы. Тишина дольше [`rtu::FRAME_GAP`]
    /// обрывок выбрасывает, и запрос после неё принят сразу.
    #[test]
    fn server_drops_a_started_frame_after_silence() {
        MockDriver::get().reset();
        let broken = [0x01, 0x10, 0, 0, 0, 1, 0xF0];
        let good = rtu_frame(1, &[0x03, 0, 0, 0, 1]);
        let mut rtu = Rtu::new(Duplex::new(&broken).waiting());
        let mut cx = Context::from_waker(Waker::noop());
        {
            let mut recv = pin!(rtu.recv());
            assert!(recv.as_mut().poll(&mut cx).is_pending());
            MockDriver::get().advance(rtu::FRAME_GAP - Duration::from_ticks(1));
            assert!(recv.as_mut().poll(&mut cx).is_pending());
        }
        assert_eq!(rtu.have, broken.len(), "до срока обрывок цел");
        {
            let mut recv = pin!(rtu.recv());
            assert!(recv.as_mut().poll(&mut cx).is_pending());
            MockDriver::get().advance(rtu::FRAME_GAP);
            assert!(recv.as_mut().poll(&mut cx).is_pending());
        }
        assert_eq!(rtu.have, 0, "обрывок выброшен");

        rtu.io.push(&good);
        let Poll::Ready(Ok(len)) = pin!(rtu.recv()).poll(&mut cx) else {
            panic!("запрос после тишины не принят");
        };
        assert_eq!(&rtu.buffer()[..len], &good[..good.len() - 2]);
    }

    /// Пауза короче [`rtu::FRAME_GAP`] кадр не рвёт, а брошенный посреди
    /// кадра приём ничего не теряет: следующий `recv` дочитывает тот же кадр.
    #[test]
    fn server_keeps_a_frame_across_a_short_pause() {
        MockDriver::get().reset();
        let good = rtu_frame(1, &[0x03, 0, 0, 0, 1]);
        let mut rtu = Rtu::new(Duplex::new(&good[..3]).waiting());
        let mut cx = Context::from_waker(Waker::noop());
        {
            let mut recv = pin!(rtu.recv());
            assert!(recv.as_mut().poll(&mut cx).is_pending());
            MockDriver::get().advance(rtu::FRAME_GAP - Duration::from_ticks(1));
            assert!(recv.as_mut().poll(&mut cx).is_pending());
        }
        rtu.io.push(&good[3..]);
        let Poll::Ready(Ok(len)) = pin!(rtu.recv()).poll(&mut cx) else {
            panic!("кадр не дочитан");
        };
        assert_eq!(&rtu.buffer()[..len], &good[..good.len() - 2]);
    }

    #[tokio::test]
    async fn server_resyncs_after_garbage_and_bad_crc() {
        let good = rtu_frame(1, &[0x03, 0, 0, 0, 1]);
        let mut broken = good.clone();
        broken[3] ^= 0xFF;
        let input = [&[0x55, 0x01, 0xAA][..], &broken, &good].concat();
        assert_eq!(requests(&input).await, [(1, vec![0x03, 0, 0, 0, 1])]);
    }

    #[tokio::test]
    async fn server_passes_any_unit_but_reserved() {
        let input = [
            rtu_frame(2, &[0x06, 0, 0, 0, 7]),
            rtu_frame(0, &[0x06, 0, 1, 0, 9]),
            rtu_frame(0xF8, &[0x06, 0, 2, 0, 1]),
        ]
        .concat();
        let units: Vec<_> = requests(&input).await.into_iter().map(|r| r.0).collect();
        assert_eq!(units, [2, 0]);
    }

    // {%- if modbus_master == "true" %}
    /// Обмен по RTU с `reply` в ответ; возвращает исход и отправленное.
    async fn rtu_exchange(
        unit: u8,
        request: &[u8],
        io: Duplex,
    ) -> (Result<Vec<u8>, ports::MasterError<Infallible>>, Vec<u8>) {
        let mut rtu = Rtu::new(io);
        let mut out = [0; MAX_PDU];
        let r = rtu.exchange(unit, request, &mut out).await;
        (r.map(|len| out[..len].to_vec()), rtu.io.tx)
    }

    #[tokio::test]
    async fn client_request_bytes_are_spec_example() {
        // Пример из спецификации: прочитать 3 регистра с 0x006B у slave'а 17.
        let (_, tx) =
            rtu_exchange(0x11, &[0x03, 0x00, 0x6B, 0x00, 0x03], Duplex::replying(&[])).await;
        assert_eq!(tx, [0x11, 0x03, 0x00, 0x6B, 0x00, 0x03, 0x76, 0x87]);
    }

    #[tokio::test]
    async fn client_stale_reply_is_drained_not_taken() {
        // Опоздавший ответ на брошенный запрос уже лежит в буфере; свой
        // приходит после отправки — его master и должен вернуть.
        let stale = rtu_frame(1, &[0x03, 2, 0, 1]);
        let fresh = rtu_frame(1, &[0x03, 2, 0, 2]);
        let io = Duplex::replying(&fresh).with_stale(&stale);
        let (got, _) = rtu_exchange(1, &[0x03, 0, 100, 0, 1], io).await;
        assert_eq!(got, Ok(vec![0x03, 2, 0, 2]));
    }

    #[tokio::test]
    async fn client_rejects_broken_replies() {
        let mut bad_crc = rtu_frame(1, &[0x03, 2, 0, 7]);
        bad_crc[3] ^= 1;
        let cases = [
            (bad_crc.clone(), ports::MasterError::Crc),
            (bad_crc[..4].to_vec(), ports::MasterError::Eof),
            // ответ другого slave'а
            (
                rtu_frame(2, &[0x03, 2, 0, 7]),
                ports::MasterError::Unexpected,
            ),
            // функция, длину ответа которой не вывести
            (rtu_frame(1, &[0x2B, 0x0E]), ports::MasterError::Unexpected),
        ];
        for (reply, error) in cases {
            let (got, _) = rtu_exchange(1, &[0x03, 0, 0, 0, 1], Duplex::replying(&reply)).await;
            assert_eq!(got, Err(error), "ответ {reply:02X?}");
        }
    }

    #[tokio::test]
    async fn broadcast_does_not_wait() {
        let mut rtu = Rtu::new(Duplex::replying(&[]));
        assert_eq!(rtu.broadcast(&[0x06, 0, 1, 0, 2]).await, Ok(()));
        assert_eq!(rtu.io.tx, rtu_frame(0, &[0x06, 0, 1, 0, 2]));
    }
    // {%- endif %}

    proptest::proptest! {
        /// Что бы ни пришло по линии — slave не паникует и кончается только
        /// концом потока.
        #[test]
        fn server_never_panics(input in proptest::collection::vec(proptest::num::u8::ANY, 0..600)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(requests(&input));
        }

        // {%- if modbus_master == "true" %}
        /// Какой бы ответ ни пришёл — master не паникует.
        #[test]
        fn client_never_panics(reply in proptest::collection::vec(proptest::num::u8::ANY, 0..300)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            let _ignored = rt.block_on(rtu_exchange(1, &[0x03, 0, 0, 0, 4], Duplex::replying(&reply)));
        }
        // {%- endif %}
    }
    // {%- endif %}

    // {%- if modbus_tcp == "true" %}
    /// Клиент на «хосте» кабеля: подключается к slave'у, пишет `request` и
    /// читает ответ длиной `reply`.
    async fn ask(client: &mut TcpSocket<'static>, request: &[u8], reply: usize) -> Vec<u8> {
        client.write_all(request).await.unwrap();
        client.flush().await.unwrap();
        let mut got = vec![0; reply];
        client.read_exact(&mut got).await.unwrap();
        got
    }

    /// Новое соединение клиента к slave'у.
    async fn connect(host: Stack<'static>) -> TcpSocket<'static> {
        let mut client = TcpSocket::new(host, leak([0; 512]), leak([0; 512]));
        client.connect((DEVICE, PORT)).await.unwrap();
        client
    }

    /// Закрывает соединение со стороны клиента. Время `mock-driver` стоит, и
    /// отложенное подтверждение `smoltcp` не уходит: ответ slave'а
    /// подтверждает следующий запрос клиента или его FIN, а FIN — сброс
    /// соединения slave'ом при конце потока (`net::tcp`, тест сервера).
    async fn close(mut client: TcpSocket<'static>) {
        client.close();
        let _reset = client.flush().await;
    }

    /// Slave над настоящим TCP-сервером на «устройстве» кабеля.
    fn slave(device: Stack<'static>) -> Tcp<Server> {
        Tcp::new(Server::new(device, PORT, leak([0; 512]), leak([0; 512])))
    }

    #[tokio::test]
    async fn server_echoes_transaction_and_unit() {
        let ((device, device_runner), (host, host_runner)) = pair();
        let mut tcp = slave(device);
        with_network(device_runner, host_runner, async {
            let device_side = async {
                // Любой адрес в MBAP — само устройство, и 0 — не широковещание.
                let len = tcp.recv().await.unwrap();
                assert_eq!(&tcp.buffer()[..len], &[0xFF, 0x03, 0, 1, 0, 1]);
                tcp.buffer()[1..5].copy_from_slice(&[0x03, 2, 0, 7]);
                tcp.send(5).await.unwrap();
                let len = tcp.recv().await.unwrap();
                assert_eq!(tcp.buffer()[0], 0xFF);
                tcp.send(len).await.unwrap();
                assert!(matches!(
                    tcp.recv().await,
                    Err(LinkError::Lost(FrameError::Eof))
                ));
            };
            let host_side = async {
                let mut client = connect(host).await;
                let first = ask(
                    &mut client,
                    &mbap_frame(0x0102, 0, 0, &[0x03, 0, 1, 0, 1]),
                    11,
                )
                .await;
                assert_eq!(first, mbap_frame(0x0102, 0, 0, &[0x03, 2, 0, 7]));
                let echo = mbap_frame(9, 0, 5, &[0x06, 0, 0, 0, 1]);
                assert_eq!(ask(&mut client, &echo, echo.len()).await, echo);
                close(client).await;
            };
            tokio::join!(device_side, host_side);
        })
        .await;
    }

    #[tokio::test]
    async fn server_skips_foreign_protocol_and_empty_frames() {
        let ((device, device_runner), (host, host_runner)) = pair();
        let mut tcp = slave(device);
        with_network(device_runner, host_runner, async {
            let device_side = async {
                assert!(matches!(tcp.recv().await, Ok(6)));
                assert_eq!(tcp.buffer()[1], 0x06);
                tcp.send(6).await.unwrap();
                assert!(tcp.recv().await.is_err(), "клиент закрыл соединение");
            };
            let host_side = async {
                let mut client = connect(host).await;
                let input = [
                    mbap_frame(1, 7, 1, &[0x03, 0, 0, 0, 1]),
                    mbap_frame(2, 0, 1, &[]),
                    mbap_frame(3, 0, 1, &[0x06, 0, 0, 0, 1]),
                ]
                .concat();
                let echo = mbap_frame(3, 0, 1, &[0x06, 0, 0, 0, 1]);
                assert_eq!(ask(&mut client, &input, echo.len()).await, echo);
                close(client).await;
            };
            tokio::join!(device_side, host_side);
        })
        .await;
    }

    /// Длина вне допустимой — slave обрывает соединение: кадр за битым по нему
    /// уже не читается, следующий `recv` — новое соединение.
    #[tokio::test]
    async fn server_hangs_up_on_bad_length() {
        let ((device, device_runner), (host, host_runner)) = pair();
        let mut tcp = slave(device);
        let mut bad_length = mbap_frame(1, 0, 1, &[0x03]);
        bad_length[5] = 0;
        let stranded = mbap_frame(2, 0, 1, &[0x06, 0, 0, 0, 1]);
        let fresh = mbap_frame(3, 0, 1, &[0x06, 0, 0, 0, 2]);
        with_network(device_runner, host_runner, async {
            let device_side = async {
                assert!(matches!(
                    tcp.recv().await,
                    Err(LinkError::Lost(FrameError::Framing))
                ));
                let len = tcp.recv().await.unwrap();
                assert_eq!(
                    &tcp.buffer()[1..len],
                    &[0x06, 0, 0, 0, 2],
                    "кадр нового соединения"
                );
                tcp.send(len).await.unwrap();
                assert!(tcp.recv().await.is_err(), "клиент закрыл соединение");
            };
            let host_side = async {
                let mut first = connect(host).await;
                first
                    .write_all(&[bad_length.as_slice(), &stranded].concat())
                    .await
                    .unwrap();
                // Ответа нет: соединение оборвано slave'ом.
                let mut buf = [0; 16];
                assert!(!matches!(first.read(&mut buf).await, Ok(n) if n > 0));
                let mut second = connect(host).await;
                assert_eq!(ask(&mut second, &fresh, fresh.len()).await, fresh);
                close(second).await;
            };
            tokio::join!(device_side, host_side);
        })
        .await;
    }

    // {%- if modbus_master == "true" %}
    async fn tcp_exchange(
        unit: u8,
        io: Duplex,
    ) -> (Result<Vec<u8>, ports::MasterError<Infallible>>, Vec<u8>) {
        let mut tcp = Tcp::new(io);
        let mut out = [0; MAX_PDU];
        let r = tcp.exchange(unit, &[0x03, 0, 0, 0, 1], &mut out).await;
        (r.map(|len| out[..len].to_vec()), tcp.io.tx)
    }

    #[tokio::test]
    async fn tcp_client_skips_stale_transactions() {
        let reply = [
            // опоздавший ответ на прошлый запрос
            mbap_frame(0, 0, 1, &[0x03, 2, 0, 1]),
            mbap_frame(1, 0, 1, &[0x03, 2, 0, 2]),
        ]
        .concat();
        let (got, tx) = tcp_exchange(1, Duplex::replying(&reply)).await;
        assert_eq!(got, Ok(vec![0x03, 2, 0, 2]));
        assert_eq!(tx, mbap_frame(1, 0, 1, &[0x03, 0, 0, 0, 1]));
    }

    #[tokio::test]
    async fn tcp_client_rejects_broken_replies() {
        let mut bad_length = mbap_frame(1, 0, 1, &[0x03]);
        bad_length[5] = 0xFF;
        let cases = [
            (
                mbap_frame(1, 0, 2, &[0x03, 2, 0, 1]),
                ports::MasterError::Unexpected,
            ),
            (bad_length, ports::MasterError::Unexpected),
            (
                mbap_frame(1, 0, 1, &[0x03, 2, 0, 1])[..9].to_vec(),
                ports::MasterError::Eof,
            ),
        ];
        for (reply, error) in cases {
            let (got, _) = tcp_exchange(1, Duplex::replying(&reply)).await;
            assert_eq!(got, Err(error), "ответ {reply:02X?}");
        }
    }

    /// Брошенный посреди приёма обмен (таймаут) не сбивает поток:
    /// следующий дочитывает недочитанный кадр, пропускает его по номеру
    /// транзакции и получает свой.
    #[tokio::test]
    async fn cancelled_exchange_keeps_stream_aligned() {
        let (stale, fresh) = (
            mbap_frame(1, 0, 1, &[0x03, 2, 0, 1]),
            mbap_frame(2, 0, 1, &[0x03, 2, 0, 2]),
        );
        let mut tcp = Tcp::new(Duplex::replying(&stale[..4]).waiting());
        let mut out = [0; MAX_PDU];
        {
            // Ответ на первый запрос пришёл до четвёртого байта, дальше —
            // тишина: один опрос, и future бросается, как на таймауте.
            let exchange = std::pin::pin!(tcp.exchange(1, &[0x03, 0, 0, 0, 1], &mut out));
            let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(exchange.poll(&mut cx).is_pending());
        }
        tcp.io.push(&[&stale[4..], &fresh[..]].concat());
        assert_eq!(tcp.exchange(1, &[0x03, 0, 0, 0, 1], &mut out).await, Ok(4));
        assert_eq!(out[..4], [0x03, 2, 0, 2]);
    }
    // {%- endif %}

    /// Оборванное чтение (конец потока) — не таймаут: недочитанный кадр
    /// сбрасывается, и поток, продолжившийся другим соединением, читается с
    /// начала кадра.
    #[tokio::test]
    async fn ended_stream_drops_the_partial_frame() {
        let ((device, device_runner), (host, host_runner)) = pair();
        let mut tcp = slave(device);
        let fresh = mbap_frame(1, 0, 7, &[0x03, 0, 1, 0, 1]);
        with_network(device_runner, host_runner, async {
            let device_side = async {
                assert!(matches!(
                    tcp.recv().await,
                    Err(LinkError::Lost(FrameError::Eof))
                ));
                let len = tcp.recv().await.unwrap();
                assert_eq!(&tcp.buffer()[1..len], &[0x03, 0, 1, 0, 1]);
                tcp.send(len).await.unwrap();
                assert!(tcp.recv().await.is_err(), "клиент закрыл соединение");
            };
            let host_side = async {
                let mut first = connect(host).await;
                first.write_all(&fresh[..4]).await.unwrap();
                close(first).await;
                let mut second = connect(host).await;
                assert_eq!(ask(&mut second, &fresh, fresh.len()).await, fresh);
                close(second).await;
            };
            tokio::join!(device_side, host_side);
        })
        .await;
    }

    proptest::proptest! {
        /// Что бы ни пришло по соединению — разбор кадров не паникует. Разбор
        /// не зависит от потока под ним, поэтому идёт над потоком в памяти.
        #[test]
        fn tcp_never_panics(input in proptest::collection::vec(proptest::num::u8::ANY, 0..600)) {
            let rt = tokio::runtime::Builder::new_current_thread().build().unwrap();
            rt.block_on(async {
                let mut tcp = Tcp::new(Duplex::new(&input));
                while tcp.frame().await.is_ok() {}
                // {%- if modbus_master == "true" %}
                let _ignored = tcp_exchange(1, Duplex::replying(&input)).await;
                // {%- endif %}
            });
        }
    }
    // {%- endif %}

    // {%- if modbus_udp == "true" %}
    #[tokio::test]
    async fn server_skips_broken_and_replies_to_last() {
        let mut bad_length = mbap_frame(3, 0, 1, &[0x03, 0, 0, 0, 1]);
        bad_length[5] = 9;
        let mut udp = Udp::new(Datagrams::with([
            Ok(vec![0, 1, 0]),
            Ok(mbap_frame(2, 7, 1, &[0x03, 0, 0, 0, 1])),
            Ok(bad_length),
            Err(LinkError::Lost("обрезана")),
            Ok(mbap_frame(4, 0, 0, &[0x06, 0, 1, 0, 2])),
        ]));
        let len = udp.recv().await.unwrap();
        assert_eq!(&udp.buffer()[..len], &[0xFF, 0x06, 0, 1, 0, 2]);
        udp.link.send = Err(LinkError::Lost("нет буфера"));
        assert_eq!(udp.send(len).await, Err(LinkError::Lost("нет буфера")));
        udp.link.send = Ok(());
        udp.send(len).await.unwrap();
        assert_eq!(udp.link.sent, [mbap_frame(4, 0, 0, &[0x06, 0, 1, 0, 2])]);
        assert!(matches!(udp.recv().await, Err(LinkError::Down(_))));
    }

    // {%- if modbus_master == "true" %}
    #[tokio::test]
    async fn udp_client_skips_stale_transactions() {
        let mut udp = Udp::new(Datagrams::with([
            Ok(mbap_frame(0, 0, 1, &[0x03, 2, 0, 1])),
            Ok(mbap_frame(1, 0, 1, &[0x03, 2, 0, 2])),
        ]));
        let mut out = [0; MAX_PDU];
        assert_eq!(udp.exchange(1, &[0x03, 0, 0, 0, 1], &mut out).await, Ok(4));
        assert_eq!(out[..4], [0x03, 2, 0, 2]);
        assert_eq!(udp.link.sent, [mbap_frame(1, 0, 1, &[0x03, 0, 0, 0, 1])]);
    }

    #[tokio::test]
    async fn udp_client_rejects_foreign_unit_and_gives_up_on_losses() {
        let mut udp = Udp::new(Datagrams::with([Ok(mbap_frame(1, 0, 2, &[0x03, 2, 0, 1]))]));
        let mut out = [0; MAX_PDU];
        assert_eq!(
            udp.exchange(1, &[0x03, 0, 0, 0, 1], &mut out).await,
            Err(ports::MasterError::Unexpected)
        );

        let losses = (0..MAX_LOST_IN_A_ROW).map(|_| Err(LinkError::Lost("помеха")));
        let mut udp = Udp::new(Datagrams::with(losses.chain([Ok(mbap_frame(
            1,
            0,
            1,
            &[0x03, 2, 0, 1],
        ))])));
        assert_eq!(
            udp.exchange(1, &[0x03, 0, 0, 0, 1], &mut out).await,
            Err(ports::MasterError::Io(LinkError::Lost("помеха")))
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
        fn udp_never_panics(datagram in proptest::collection::vec(proptest::num::u8::ANY, 0..300)) {
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
    // {%- endif %}
}
