//! Периферия, которой ещё нет на плате, — заглушки в слотах драйверов.
//!
//! Шаблон не выбирает выводы за проект (`docs/architecture.md`): какой блок и
//! на каких ножках, знает только плата. Пока драйвер не подставлен, в слоте
//! стоит заглушка: проект с ней собирается и запускается, приём ждёт вечно (и
//! узел спит, не занимая исполнитель), а передача молча выбрасывает данные.
//! Заглушка — часть платы, а не приложения: «железо, которого пока нет».
//! Заменить её — поменять псевдоним в модуле блока (`bsp::serial`, `bsp::can`,
//! …) на драйвер `embassy-stm32`; рецепт — `docs/modules/bsp-*.md`.
{%- if has_uart == "true" %}

/// UART, который ещё не подключили, — место под `BufferedUart` платы.
#[derive(Debug, Default)]
pub struct Serial;

impl embedded_io_async::ErrorType for Serial {
    type Error = core::convert::Infallible;
}

impl embedded_io_async::Read for Serial {
    async fn read(&mut self, _: &mut [u8]) -> Result<usize, Self::Error> {
        core::future::pending().await
    }
}

impl embedded_io_async::ReadReady for Serial {
    fn read_ready(&mut self) -> Result<bool, Self::Error> {
        Ok(false)
    }
}

impl embedded_io_async::Write for Serial {
    fn write(&mut self, buf: &[u8]) -> impl Future<Output = Result<usize, Self::Error>> {
        core::future::ready(Ok(buf.len()))
    }

    fn flush(&mut self) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Ok(()))
    }
}
{%- endif %}
{%- if has_can == "true" %}

/// CAN, который ещё не подключили, — место под контроллер платы. Кадр — тот
/// же тип, что у драйвера `embassy-stm32`, поэтому замена — один псевдоним.
#[derive(Debug, Default)]
pub struct Can;

impl ports::CanBus for Can {
    type Frame = embassy_stm32::can::Frame;
    type Error = core::convert::Infallible;

    async fn recv(&mut self) -> Result<Self::Frame, ports::LinkError<Self::Error>> {
        core::future::pending().await
    }

    fn send(
        &mut self,
        _: &Self::Frame,
    ) -> impl Future<Output = Result<(), ports::LinkError<Self::Error>>> {
        core::future::ready(Ok(()))
    }
}
{%- endif %}
{%- if has_usb == "true" %}

/// USB-блок, которого ещё нет на плате, — место под драйвер `embassy-stm32`.
///
/// Устройство `embassy-usb` и класс CDC над ним настоящие, а шина никогда не
/// просыпается — хост устройство не видит.
#[derive(Debug, Default)]
pub struct UsbDriver {
    /// Сколько конечных точек уже выдано — для их адресов.
    endpoints: usize,
}

/// Конечная точка, по которой ничего не приходит и не уходит.
#[derive(Debug)]
pub struct NoEndpoint(embassy_usb::driver::EndpointInfo);

/// Шина и управляющий канал, на которых не происходит ничего.
#[derive(Debug)]
pub struct NoBus;

/// Пакет управляющей точки по умолчанию — 64 байта (full-speed).
const CONTROL_PACKET: usize = 64;

impl UsbDriver {
    fn endpoint(
        &mut self,
        dir: embassy_usb::driver::Direction,
        ep_type: embassy_usb::driver::EndpointType,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> NoEndpoint {
        self.endpoints += 1;
        NoEndpoint(embassy_usb::driver::EndpointInfo {
            addr: embassy_usb::driver::EndpointAddress::from_parts(self.endpoints, dir),
            ep_type,
            max_packet_size,
            interval_ms,
        })
    }
}

impl embassy_usb::driver::Driver<'_> for UsbDriver {
    type EndpointOut = NoEndpoint;
    type EndpointIn = NoEndpoint;
    type ControlPipe = NoBus;
    type Bus = NoBus;

    fn alloc_endpoint_out(
        &mut self,
        ep_type: embassy_usb::driver::EndpointType,
        _: Option<embassy_usb::driver::EndpointAddress>,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> Result<NoEndpoint, embassy_usb::driver::EndpointAllocError> {
        Ok(self.endpoint(
            embassy_usb::driver::Direction::Out,
            ep_type,
            max_packet_size,
            interval_ms,
        ))
    }

    fn alloc_endpoint_in(
        &mut self,
        ep_type: embassy_usb::driver::EndpointType,
        _: Option<embassy_usb::driver::EndpointAddress>,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> Result<NoEndpoint, embassy_usb::driver::EndpointAllocError> {
        Ok(self.endpoint(
            embassy_usb::driver::Direction::In,
            ep_type,
            max_packet_size,
            interval_ms,
        ))
    }

    fn start(self, _: u16) -> (NoBus, NoBus) {
        (NoBus, NoBus)
    }
}

impl embassy_usb::driver::Endpoint for NoEndpoint {
    fn info(&self) -> &embassy_usb::driver::EndpointInfo {
        &self.0
    }

    async fn wait_enabled(&mut self) {
        core::future::pending::<()>().await;
    }
}

impl embassy_usb::driver::EndpointOut for NoEndpoint {
    async fn read(&mut self, _: &mut [u8]) -> Result<usize, embassy_usb::driver::EndpointError> {
        core::future::pending().await
    }
}

impl embassy_usb::driver::EndpointIn for NoEndpoint {
    async fn write(&mut self, _: &[u8]) -> Result<(), embassy_usb::driver::EndpointError> {
        core::future::pending().await
    }
}

impl embassy_usb::driver::Bus for NoBus {
    fn enable(&mut self) -> impl Future<Output = ()> {
        core::future::ready(())
    }

    fn disable(&mut self) -> impl Future<Output = ()> {
        core::future::ready(())
    }

    async fn poll(&mut self) -> embassy_usb::driver::Event {
        core::future::pending().await
    }

    fn endpoint_set_enabled(&mut self, _: embassy_usb::driver::EndpointAddress, _: bool) {}

    fn endpoint_set_stalled(&mut self, _: embassy_usb::driver::EndpointAddress, _: bool) {}

    fn endpoint_is_stalled(&mut self, _: embassy_usb::driver::EndpointAddress) -> bool {
        false
    }

    fn remote_wakeup(
        &mut self,
    ) -> impl Future<Output = Result<(), embassy_usb::driver::Unsupported>> {
        core::future::ready(Err(embassy_usb::driver::Unsupported))
    }
}

impl embassy_usb::driver::ControlPipe for NoBus {
    fn max_packet_size(&self) -> usize {
        CONTROL_PACKET
    }

    async fn setup(&mut self) -> [u8; 8] {
        core::future::pending().await
    }

    async fn data_out(
        &mut self,
        _: &mut [u8],
        _: bool,
        _: bool,
    ) -> Result<usize, embassy_usb::driver::EndpointError> {
        core::future::pending().await
    }

    async fn data_in(
        &mut self,
        _: &[u8],
        _: bool,
        _: bool,
    ) -> Result<(), embassy_usb::driver::EndpointError> {
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
{%- endif %}
{%- if has_net == "true" %}

/// Сетевое устройство, которого ещё нет на плате, — место под Ethernet.
///
/// Стек `embassy-net` над ним настоящий, а линк всегда опущен: кадров нет ни
/// на приёме, ни на передаче.
#[derive(Debug)]
pub struct NetDevice {
    mac: [u8; 6],
}

impl NetDevice {
    /// Устройство с адресом `mac` — его увидит стек, хотя в сеть ничего не
    /// уходит.
    #[must_use]
    pub const fn new(mac: [u8; 6]) -> Self {
        Self { mac }
    }
}

/// Кадр, которого не бывает: токены приёма и передачи [`NetDevice`].
#[derive(Debug)]
pub enum NoFrame {}

impl embassy_net::driver::RxToken for NoFrame {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, _: F) -> R {
        match self {}
    }
}

impl embassy_net::driver::TxToken for NoFrame {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, _: usize, _: F) -> R {
        match self {}
    }
}

/// MTU Ethernet: 1500 байт IP и 14 байт заголовка.
const ETHERNET_MTU: usize = 1514;

impl embassy_net::driver::Driver for NetDevice {
    type RxToken<'a> = NoFrame;
    type TxToken<'a> = NoFrame;

    fn receive(&mut self, _: &mut core::task::Context) -> Option<(NoFrame, NoFrame)> {
        None
    }

    fn transmit(&mut self, _: &mut core::task::Context) -> Option<NoFrame> {
        None
    }

    fn link_state(&mut self, _: &mut core::task::Context) -> embassy_net::driver::LinkState {
        embassy_net::driver::LinkState::Down
    }

    fn capabilities(&self) -> embassy_net::driver::Capabilities {
        let mut capabilities = embassy_net::driver::Capabilities::default();
        capabilities.max_transmission_unit = ETHERNET_MTU;
        capabilities
    }

    fn hardware_address(&self) -> embassy_net::driver::HardwareAddress {
        embassy_net::driver::HardwareAddress::Ethernet(self.mac)
    }
}
{%- endif %}
{%- if external_flash == "true" %}

/// Внешняя флеш, которую ещё не подключили, — место под ваш драйвер.
///
/// Шаблон не знает, какая микросхема будет на плате (W25Q, GD25, MX25, IS25,
/// SST26…), и не гадает. Каждая операция отвечает [`NotConnected`] —
/// настройки не сохраняются, образ не принимается, но ничего не падает.
/// Геометрия правдоподобная — две страницы по 4 KiB, сектор почти любой SPI
/// NOR. Асинхронные трейты — для приложения, блокирующие — для bootloader'а;
/// bootloader с заглушкой не обменивает разделы, а загружает текущий образ.
#[derive(Debug, Default)]
pub struct Flash;

impl Flash {
    /// Объём, под который строится хранилище с заглушкой.
    pub const CAPACITY: u32 = 2 * Self::PAGE;

    /// Страница стирания.
    const PAGE: u32 = 4096;
}

/// Ответ [`Flash`] на любую операцию.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub struct NotConnected;

impl embedded_storage::nor_flash::NorFlashError for NotConnected {
    fn kind(&self) -> embedded_storage::nor_flash::NorFlashErrorKind {
        embedded_storage::nor_flash::NorFlashErrorKind::Other
    }
}

impl embedded_storage::nor_flash::ErrorType for Flash {
    type Error = NotConnected;
}

impl embedded_storage_async::nor_flash::ReadNorFlash for Flash {
    const READ_SIZE: usize = 1;

    fn read(&mut self, _: u32, _: &mut [u8]) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Err(NotConnected))
    }

    fn capacity(&self) -> usize {
        Self::CAPACITY as usize
    }
}

impl embedded_storage_async::nor_flash::NorFlash for Flash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = Self::PAGE as usize;

    fn erase(&mut self, _: u32, _: u32) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Err(NotConnected))
    }

    fn write(&mut self, _: u32, _: &[u8]) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Err(NotConnected))
    }
}

impl embedded_storage::nor_flash::ReadNorFlash for Flash {
    const READ_SIZE: usize = 1;

    fn read(&mut self, _: u32, _: &mut [u8]) -> Result<(), Self::Error> {
        Err(NotConnected)
    }

    fn capacity(&self) -> usize {
        Self::CAPACITY as usize
    }
}

impl embedded_storage::nor_flash::NorFlash for Flash {
    const WRITE_SIZE: usize = 1;
    const ERASE_SIZE: usize = Self::PAGE as usize;

    fn erase(&mut self, _: u32, _: u32) -> Result<(), Self::Error> {
        Err(NotConnected)
    }

    fn write(&mut self, _: u32, _: &[u8]) -> Result<(), Self::Error> {
        Err(NotConnected)
    }
}
{%- endif %}
