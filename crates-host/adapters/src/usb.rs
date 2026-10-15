//! USB поверх `embassy-usb`: заглушка USB-драйвера и поток CDC ACM.
//!
//! Чип здесь не нужен: драйвер — любой [`Driver`](embassy_usb::driver::Driver),
//! класс CDC — стек `embassy-usb`. `bsp` только собирает устройство над
//! своим USB-блоком и отдаёт эти объекты в `Board`.

use core::convert::Infallible;

use embassy_time::{Duration, with_timeout};
use embassy_usb::class::cdc_acm::{BufferedReceiver, CdcAcmError, Sender};
use embassy_usb::driver::{
    Bus, ControlPipe, Direction, Driver, Endpoint, EndpointAddress, EndpointAllocError,
    EndpointError, EndpointIn, EndpointInfo, EndpointOut, EndpointType, Event, Unsupported,
};
use embedded_io_async::{ErrorType, Read, Write};

/// USB-блок, которого ещё нет на плате, — место под драйвер `embassy-stm32`.
///
/// Как `net::Unconnected` у сети: устройство `embassy-usb`, класс CDC и узел
/// `USB` настоящие, а шина никогда не просыпается — хост устройство не
/// видит. Проект с ней собирается и запускается, протокол ждёт хоста
/// вечно. Заменить её — значит в `bsp::usb` подставить драйвер своей платы.
#[derive(Debug, Default)]
pub struct Unconnected {
    /// Сколько конечных точек уже выдано — для их адресов.
    endpoints: usize,
}

/// Конечная точка, по которой ничего не приходит и не уходит.
#[derive(Debug)]
pub struct NoEndpoint(EndpointInfo);

/// Шина и управляющий канал, на которых не происходит ничего.
#[derive(Debug)]
pub struct NoBus;

impl Unconnected {
    fn endpoint(
        &mut self,
        dir: Direction,
        ep_type: EndpointType,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> NoEndpoint {
        self.endpoints += 1;
        NoEndpoint(EndpointInfo {
            addr: EndpointAddress::from_parts(self.endpoints, dir),
            ep_type,
            max_packet_size,
            interval_ms,
        })
    }
}

impl Driver<'_> for Unconnected {
    type EndpointOut = NoEndpoint;
    type EndpointIn = NoEndpoint;
    type ControlPipe = NoBus;
    type Bus = NoBus;

    fn alloc_endpoint_out(
        &mut self,
        ep_type: EndpointType,
        _: Option<EndpointAddress>,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> Result<NoEndpoint, EndpointAllocError> {
        Ok(self.endpoint(Direction::Out, ep_type, max_packet_size, interval_ms))
    }

    fn alloc_endpoint_in(
        &mut self,
        ep_type: EndpointType,
        _: Option<EndpointAddress>,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> Result<NoEndpoint, EndpointAllocError> {
        Ok(self.endpoint(Direction::In, ep_type, max_packet_size, interval_ms))
    }

    fn start(self, _: u16) -> (NoBus, NoBus) {
        (NoBus, NoBus)
    }
}

impl Endpoint for NoEndpoint {
    fn info(&self) -> &EndpointInfo {
        &self.0
    }

    async fn wait_enabled(&mut self) {
        core::future::pending::<()>().await;
    }
}

impl EndpointOut for NoEndpoint {
    async fn read(&mut self, _: &mut [u8]) -> Result<usize, EndpointError> {
        core::future::pending().await
    }
}

impl EndpointIn for NoEndpoint {
    async fn write(&mut self, _: &[u8]) -> Result<(), EndpointError> {
        core::future::pending().await
    }
}

impl Bus for NoBus {
    fn enable(&mut self) -> impl Future<Output = ()> {
        core::future::ready(())
    }

    fn disable(&mut self) -> impl Future<Output = ()> {
        core::future::ready(())
    }

    async fn poll(&mut self) -> Event {
        core::future::pending().await
    }

    fn endpoint_set_enabled(&mut self, _: EndpointAddress, _: bool) {}

    fn endpoint_set_stalled(&mut self, _: EndpointAddress, _: bool) {}

    fn endpoint_is_stalled(&mut self, _: EndpointAddress) -> bool {
        false
    }

    fn remote_wakeup(&mut self) -> impl Future<Output = Result<(), Unsupported>> {
        core::future::ready(Err(Unsupported))
    }
}

/// Пакет управляющей точки по умолчанию — 64 байта (full-speed).
const CONTROL_PACKET: usize = 64;

impl ControlPipe for NoBus {
    fn max_packet_size(&self) -> usize {
        CONTROL_PACKET
    }

    async fn setup(&mut self) -> [u8; 8] {
        core::future::pending().await
    }

    async fn data_out(&mut self, _: &mut [u8], _: bool, _: bool) -> Result<usize, EndpointError> {
        core::future::pending().await
    }

    async fn data_in(&mut self, _: &[u8], _: bool, _: bool) -> Result<(), EndpointError> {
        core::future::pending().await
    }

    fn accept(&mut self) -> impl Future<Output = ()> {
        core::future::ready(())
    }

    fn reject(&mut self) -> impl Future<Output = ()> {
        core::future::ready(())
    }

    fn accept_set_address(&mut self, _: u8) -> impl Future<Output = ()> {
        core::future::ready(())
    }
}

/// Поток байтов поверх класса CDC ACM («виртуальный COM-порт») — для
/// транспортов протоколов (`cobs::Cobs`).
///
/// Отключение хоста (кабель вынут) потоком не кончается: чтение ждёт, пока
/// хост не подключится снова, — узел протокола не перезапускается от
/// каждого отключения. Хвост кадра, недочитанного до отключения, транспорт
/// протокола отбросит сам при разборе первого кадра новой сессии (COBS — по
/// разделителю `0x00`, RTU — по CRC), так что первый запрос после
/// переподключения может потеряться.
///
/// Ответ, который никто не забирает (кабель вынут или порт закрыт, хотя
/// устройство на шине), отбрасывается по [`WRITE_TIMEOUT`]: иначе запись
/// ждала бы хоста вечно, а узел протокола стоял бы.
pub struct CdcStream<'d, D: Driver<'d>> {
    tx: Sender<'d, D>,
    rx: BufferedReceiver<'d, D>,
    /// Последний отправленный пакет полный: передачу надо закрыть пакетом
    /// нулевой длины, иначе хост ждёт продолжения и не отдаёт её приложению.
    unterminated: bool,
}

/// Сколько пакет ждёт, пока хост его заберёт. Пакет 64 байта хост забирает
/// за миллисекунду; не забрал за 100 — порт закрыт, и ждать нечего.
pub const WRITE_TIMEOUT: Duration = Duration::from_millis(100);

impl<'d, D: Driver<'d>> CdcStream<'d, D> {
    /// Поток из двух половин класса: `CdcAcmClass::split` и
    /// `Receiver::into_buffered`.
    #[must_use]
    pub const fn new(tx: Sender<'d, D>, rx: BufferedReceiver<'d, D>) -> Self {
        Self {
            tx,
            rx,
            unterminated: false,
        }
    }

    /// Отправляет пакет; не забранный хостом или отключённым — отбрасывает.
    async fn send_packet(&mut self, packet: &[u8]) {
        if let Ok(Ok(())) = with_timeout(WRITE_TIMEOUT, self.tx.write_packet(packet)).await {
            self.unterminated = packet.len() == usize::from(self.tx.max_packet_size());
        } else {
            self.unterminated = false;
        }
    }
}

// Вручную: у половин класса `embassy-usb` нет `Debug`.
impl<'d, D: Driver<'d>> core::fmt::Debug for CdcStream<'d, D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CdcStream").finish_non_exhaustive()
    }
}

impl<'d, D: Driver<'d>> ErrorType for CdcStream<'d, D> {
    type Error = Infallible;
}

impl<'d, D: Driver<'d>> Read for CdcStream<'d, D> {
    async fn read(&mut self, buf: &mut [u8]) -> Result<usize, Infallible> {
        loop {
            match self.rx.read(buf).await {
                Ok(n) => return Ok(n),
                Err(CdcAcmError::NotConnected) => self.rx.wait_connection().await,
            }
        }
    }
}

impl<'d, D: Driver<'d>> Write for CdcStream<'d, D> {
    async fn write(&mut self, buf: &[u8]) -> Result<usize, Infallible> {
        let len = buf.len().min(usize::from(self.tx.max_packet_size()));
        self.send_packet(&buf[..len]).await;
        Ok(len)
    }

    /// Закрывает передачу: после полного пакета — пакет нулевой длины.
    async fn flush(&mut self) -> Result<(), Infallible> {
        if self.unterminated {
            self.send_packet(&[]).await;
        }
        Ok(())
    }
}
