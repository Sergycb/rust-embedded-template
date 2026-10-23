//! USB поверх `embassy-usb`: поток CDC ACM и сборка устройства с ним.
//!
//! Чип здесь не нужен: драйвер — любой [`Driver`](embassy_usb::driver::Driver),
//! класс CDC — стек `embassy-usb`. `bsp` только собирает устройство над
//! своим USB-блоком и отдаёт эти объекты в `Board`.

use core::convert::Infallible;

use embassy_time::{Duration, with_timeout};
use embassy_usb::class::cdc_acm::{BufferedReceiver, CdcAcmClass, CdcAcmError, Sender, State};
use embassy_usb::driver::Driver;
use embassy_usb::{Builder, Config, UsbDevice};
use embedded_io_async::{ErrorType, Read, Write};

/// Сколько пакет ждёт, пока хост его заберёт. Пакет 64 байта хост забирает
/// за миллисекунду; не забрал за 100 — порт закрыт, и ждать нечего.
pub const WRITE_TIMEOUT: Duration = Duration::from_millis(100);

/// Пакет CDC — 64 байта: предел full-speed для bulk-точек.
pub const PACKET: u16 = 64;

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

/// Память под устройство CDC ACM: `Builder` и класс держат её `&'static mut`.
///
/// Отдельной структурой, а не россыпью `static` на стороне платы: сборка
/// устройства от чипа не зависит, и плата даёт ей только место. Состояние
/// класса ([`State`]) сюда не входит: оно не `Send` (внутри ссылка на
/// `RefCell`), а `ConstStaticCell`, в котором плата хранит эту структуру, `Send`
/// требует.
#[derive(Debug)]
pub struct CdcBuffers {
    config_descriptor: [u8; 256],
    bos_descriptor: [u8; 256],
    control: [u8; 64],
    rx: [u8; PACKET as usize],
}

impl CdcBuffers {
    /// Нулевые буферы — для `static`.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            config_descriptor: [0; 256],
            bos_descriptor: [0; 256],
            control: [0; 64],
            rx: [0; PACKET as usize],
        }
    }
}

impl Default for CdcBuffers {
    fn default() -> Self {
        Self::new()
    }
}

/// USB платы: устройство для узла `USB` и поток CDC под протоколом.
///
/// Пара, потому что обе половины делает один `Builder`; чипа она не знает —
/// `D` подставляет плата.
pub struct Usb<D: Driver<'static>> {
    /// USB-устройство — уезжает в узел `USB` графа.
    pub device: UsbDevice<'static, D>,
    /// Поток CDC ACM («виртуальный COM-порт») — под транспортом протокола.
    pub cdc: CdcStream<'static, D>,
}

// Вручную: у `UsbDevice` нет `Debug`.
impl<D: Driver<'static>> core::fmt::Debug for Usb<D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Usb").finish_non_exhaustive()
    }
}

/// Собирает USB-устройство с одним классом CDC ACM над драйвером платы:
/// устройство уезжает в узел `USB`, поток — под транспорт протокола.
#[must_use]
pub fn cdc<D: Driver<'static>>(
    driver: D,
    config: Config<'static>,
    buffers: &'static mut CdcBuffers,
    state: &'static mut State<'static>,
) -> Usb<D> {
    let CdcBuffers {
        config_descriptor,
        bos_descriptor,
        control,
        rx,
    } = buffers;
    let mut builder = Builder::new(
        driver,
        config,
        config_descriptor,
        bos_descriptor,
        &mut [],
        control,
    );
    let class = CdcAcmClass::new(&mut builder, state, PACKET);
    let device = builder.build();
    let (tx, rx_half) = class.split();
    Usb {
        device,
        cdc: CdcStream::new(tx, rx_half.into_buffered(rx)),
    }
}

#[cfg(test)]
mod tests {
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use embassy_time::MockDriver;
    use embassy_usb::class::cdc_acm::State;
    use embassy_usb::driver::{
        Direction, Endpoint, EndpointAddress, EndpointAllocError, EndpointError, EndpointIn,
        EndpointInfo, EndpointOut, EndpointType,
    };

    use super::*;
    use crate::fake::NoBus;

    /// Что делает хост — общее состояние всех точек сценарного драйвера.
    #[derive(Default)]
    struct Host {
        /// Порт открыт: точки включены.
        enabled: bool,
        /// Пакеты от хоста; `None` — отключение посреди сессии.
        sent: VecDeque<Option<Vec<u8>>>,
        /// Пакеты, которые хост забрал.
        taken: Vec<Vec<u8>>,
        /// Хост пакеты не забирает — запись ждёт.
        stalled: bool,
    }

    type Shared = Rc<RefCell<Host>>;

    /// Драйвер, точки которого делают то, что велит [`Host`]. Waker'а точки
    /// не запоминают: ждущий future тест поллит сам, после того как поменял
    /// [`Host`], — `.await` на нём повис бы.
    struct Scripted(Shared);

    struct Point(Shared, EndpointInfo);

    impl Endpoint for Point {
        fn info(&self) -> &EndpointInfo {
            &self.1
        }

        async fn wait_enabled(&mut self) {
            core::future::poll_fn(|_| {
                if self.0.borrow().enabled {
                    Poll::Ready(())
                } else {
                    Poll::Pending
                }
            })
            .await;
        }
    }

    impl EndpointOut for Point {
        async fn read(&mut self, buf: &mut [u8]) -> Result<usize, EndpointError> {
            core::future::poll_fn(|_| {
                let mut host = self.0.borrow_mut();
                match host.sent.pop_front() {
                    Some(Some(packet)) => {
                        buf[..packet.len()].copy_from_slice(&packet);
                        Poll::Ready(Ok(packet.len()))
                    }
                    Some(None) => {
                        host.enabled = false;
                        Poll::Ready(Err(EndpointError::Disabled))
                    }
                    None => Poll::Pending,
                }
            })
            .await
        }
    }

    impl EndpointIn for Point {
        async fn write(&mut self, buf: &[u8]) -> Result<(), EndpointError> {
            core::future::poll_fn(|_| {
                let mut host = self.0.borrow_mut();
                if host.stalled {
                    Poll::Pending
                } else {
                    host.taken.push(buf.to_vec());
                    Poll::Ready(Ok(()))
                }
            })
            .await
        }
    }

    impl Scripted {
        fn point(&self, dir: Direction, ep_type: EndpointType, max_packet_size: u16) -> Point {
            let index = match (dir, ep_type) {
                (Direction::In, EndpointType::Interrupt) => 1,
                _ => 2,
            };
            Point(
                Rc::clone(&self.0),
                EndpointInfo {
                    addr: EndpointAddress::from_parts(index, dir),
                    ep_type,
                    max_packet_size,
                    interval_ms: 0,
                },
            )
        }
    }

    impl Driver<'static> for Scripted {
        type EndpointOut = Point;
        type EndpointIn = Point;
        type ControlPipe = NoBus;
        type Bus = NoBus;

        fn alloc_endpoint_out(
            &mut self,
            ep_type: EndpointType,
            _: Option<EndpointAddress>,
            max_packet_size: u16,
            _: u8,
        ) -> Result<Point, EndpointAllocError> {
            Ok(self.point(Direction::Out, ep_type, max_packet_size))
        }

        fn alloc_endpoint_in(
            &mut self,
            ep_type: EndpointType,
            _: Option<EndpointAddress>,
            max_packet_size: u16,
            _: u8,
        ) -> Result<Point, EndpointAllocError> {
            Ok(self.point(Direction::In, ep_type, max_packet_size))
        }

        fn start(self, _: u16) -> (NoBus, NoBus) {
            (NoBus, NoBus)
        }
    }

    fn leak<T>(value: T) -> &'static mut T {
        Box::leak(Box::new(value))
    }

    /// Поток CDC над сценарным драйвером и хост, который им управляет.
    fn stream() -> (CdcStream<'static, Scripted>, Shared) {
        let host = Shared::default();
        let Usb { cdc: stream, .. } = cdc(
            Scripted(Rc::clone(&host)),
            embassy_usb::Config::new(0xC0DE, 0xCAFE),
            leak(CdcBuffers::new()),
            leak(State::new()),
        );
        (stream, host)
    }

    /// Чтение отдаёт пакеты хоста, а отключение потоком не кончается: поток
    /// ждёт, пока хост снова откроет порт, и читает дальше.
    #[tokio::test]
    async fn read_survives_a_reconnect() {
        let (mut stream, host) = stream();
        host.borrow_mut().enabled = true;
        host.borrow_mut()
            .sent
            .extend([Some(vec![1, 2]), None, Some(vec![3])]);

        let mut buf = [0; 32];
        assert_eq!(stream.read(&mut buf).await, Ok(2));
        assert_eq!(buf[..2], [1, 2]);

        let mut cx = Context::from_waker(Waker::noop());
        {
            let mut read = pin!(stream.read(&mut buf));
            assert!(read.as_mut().poll(&mut cx).is_pending(), "ждёт хоста");
            host.borrow_mut().enabled = true;
            assert_eq!(read.poll(&mut cx), Poll::Ready(Ok(1)));
        }
        assert_eq!(buf[0], 3);
    }

    /// Запись режет буфер по пакету, а полный последний пакет сброс
    /// закрывает пакетом нулевой длины — неполный не закрывает.
    #[tokio::test]
    async fn full_packet_is_terminated_on_flush() {
        let (mut stream, host) = stream();
        host.borrow_mut().enabled = true;

        assert_eq!(stream.write(&[7; 70]).await, Ok(usize::from(PACKET)));
        stream.flush().await.unwrap();
        assert_eq!(stream.write(&[8; 4]).await, Ok(4));
        stream.flush().await.unwrap();

        let taken = host.borrow().taken.clone();
        assert_eq!(taken, [vec![7; usize::from(PACKET)], vec![], vec![8; 4]]);
    }

    /// Пакет, который хост не забирает, отбрасывается по таймауту — запись
    /// не стоит вечно, и неотправленный полный пакет сброс не закрывает.
    #[tokio::test]
    async fn untaken_packet_is_dropped_after_timeout() {
        let (mut stream, host) = stream();
        host.borrow_mut().stalled = true;

        let mut cx = Context::from_waker(Waker::noop());
        {
            let mut write = pin!(stream.write(&[1; PACKET as usize]));
            assert!(write.as_mut().poll(&mut cx).is_pending());
            MockDriver::get().advance(WRITE_TIMEOUT);
            assert_eq!(write.poll(&mut cx), Poll::Ready(Ok(usize::from(PACKET))));
        }

        host.borrow_mut().stalled = false;
        stream.flush().await.unwrap();
        assert!(host.borrow().taken.is_empty(), "{:?}", host.borrow().taken);
    }
}
