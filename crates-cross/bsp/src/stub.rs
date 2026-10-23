//! Периферия, которой ещё нет на плате, — заглушки в слотах драйверов.
//!
//! Шаблон не выбирает выводы за проект (`docs/architecture.md`): какой блок и
//! на каких ножках, знает только плата. Пока драйвер не подставлен, в слоте
//! стоит заглушка: проект с ней собирается и запускается, приём ждёт вечно (и
//! узел спит, не занимая исполнитель), а передача молча выбрасывает данные.
//! Заглушка — часть платы, а не приложения: «железо, которого пока нет».
//! Заменить её — поменять псевдоним в модуле блока (`bsp::serial`, `bsp::can`,
//! …) на драйвер `embassy-stm32`; рецепт — `docs/modules/bsp-*.md`.
{%- if has_usb == "true" %}

/// Пакет управляющей точки по умолчанию — 64 байта (full-speed).
const CONTROL_PACKET: usize = 64;
{%- endif %}
{%- if has_net == "true" %}

/// MTU Ethernet: 1500 байт IP и 14 байт заголовка.
const ETHERNET_MTU: usize = 1514;
{%- endif %}
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
pub struct NoEndpoint(embassy_usb_driver::EndpointInfo);

/// Шина и управляющий канал, на которых не происходит ничего.
#[derive(Debug)]
pub struct NoBus;

impl UsbDriver {
    fn endpoint(
        &mut self,
        dir: embassy_usb_driver::Direction,
        ep_type: embassy_usb_driver::EndpointType,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> NoEndpoint {
        self.endpoints += 1;
        NoEndpoint(embassy_usb_driver::EndpointInfo {
            addr: embassy_usb_driver::EndpointAddress::from_parts(self.endpoints, dir),
            ep_type,
            max_packet_size,
            interval_ms,
        })
    }
}

impl embassy_usb_driver::Driver<'_> for UsbDriver {
    type EndpointOut = NoEndpoint;
    type EndpointIn = NoEndpoint;
    type ControlPipe = NoBus;
    type Bus = NoBus;

    fn alloc_endpoint_out(
        &mut self,
        ep_type: embassy_usb_driver::EndpointType,
        _: Option<embassy_usb_driver::EndpointAddress>,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> Result<NoEndpoint, embassy_usb_driver::EndpointAllocError> {
        Ok(self.endpoint(
            embassy_usb_driver::Direction::Out,
            ep_type,
            max_packet_size,
            interval_ms,
        ))
    }

    fn alloc_endpoint_in(
        &mut self,
        ep_type: embassy_usb_driver::EndpointType,
        _: Option<embassy_usb_driver::EndpointAddress>,
        max_packet_size: u16,
        interval_ms: u8,
    ) -> Result<NoEndpoint, embassy_usb_driver::EndpointAllocError> {
        Ok(self.endpoint(
            embassy_usb_driver::Direction::In,
            ep_type,
            max_packet_size,
            interval_ms,
        ))
    }

    fn start(self, _: u16) -> (NoBus, NoBus) {
        (NoBus, NoBus)
    }
}

impl embassy_usb_driver::Endpoint for NoEndpoint {
    fn info(&self) -> &embassy_usb_driver::EndpointInfo {
        &self.0
    }

    async fn wait_enabled(&mut self) {
        core::future::pending::<()>().await;
    }
}

impl embassy_usb_driver::EndpointOut for NoEndpoint {
    async fn read(&mut self, _: &mut [u8]) -> Result<usize, embassy_usb_driver::EndpointError> {
        core::future::pending().await
    }
}

impl embassy_usb_driver::EndpointIn for NoEndpoint {
    async fn write(&mut self, _: &[u8]) -> Result<(), embassy_usb_driver::EndpointError> {
        core::future::pending().await
    }
}

impl embassy_usb_driver::Bus for NoBus {
    fn enable(&mut self) -> impl Future<Output = ()> {
        core::future::ready(())
    }

    fn disable(&mut self) -> impl Future<Output = ()> {
        core::future::ready(())
    }

    async fn poll(&mut self) -> embassy_usb_driver::Event {
        core::future::pending().await
    }

    fn endpoint_set_enabled(&mut self, _: embassy_usb_driver::EndpointAddress, _: bool) {}

    fn endpoint_set_stalled(&mut self, _: embassy_usb_driver::EndpointAddress, _: bool) {}

    fn endpoint_is_stalled(&mut self, _: embassy_usb_driver::EndpointAddress) -> bool {
        false
    }

    fn remote_wakeup(
        &mut self,
    ) -> impl Future<Output = Result<(), embassy_usb_driver::Unsupported>> {
        core::future::ready(Err(embassy_usb_driver::Unsupported))
    }
}

impl embassy_usb_driver::ControlPipe for NoBus {
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
    ) -> Result<usize, embassy_usb_driver::EndpointError> {
        core::future::pending().await
    }

    async fn data_in(
        &mut self,
        _: &[u8],
        _: bool,
        _: bool,
    ) -> Result<(), embassy_usb_driver::EndpointError> {
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

impl embassy_net_driver::RxToken for NoFrame {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, _: F) -> R {
        match self {}
    }
}

impl embassy_net_driver::TxToken for NoFrame {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, _: usize, _: F) -> R {
        match self {}
    }
}

impl embassy_net_driver::Driver for NetDevice {
    type RxToken<'a> = NoFrame;
    type TxToken<'a> = NoFrame;

    fn receive(&mut self, _: &mut core::task::Context) -> Option<(NoFrame, NoFrame)> {
        None
    }

    fn transmit(&mut self, _: &mut core::task::Context) -> Option<NoFrame> {
        None
    }

    fn link_state(&mut self, _: &mut core::task::Context) -> embassy_net_driver::LinkState {
        embassy_net_driver::LinkState::Down
    }

    fn capabilities(&self) -> embassy_net_driver::Capabilities {
        let mut capabilities = embassy_net_driver::Capabilities::default();
        capabilities.max_transmission_unit = ETHERNET_MTU;
        capabilities
    }

    fn hardware_address(&self) -> embassy_net_driver::HardwareAddress {
        embassy_net_driver::HardwareAddress::Ethernet(self.mac)
    }
}
{%- endif %}
{%- if external_flash == "true" %}

/// Шина внешней флеши, к которой ещё не подключили микросхему, — место под
/// {% if ext_flash_bus == "spi" %}SPI{% else %}QSPI{% endif %} платы (`bsp::external::Bus`).
///
/// Драйвер W25Q над ней настоящий, а каждая команда отвечает
/// [`NotConnected`]: `init` не проходит, и драйвер отказывает в любой
/// операции — настройки не сохраняются, образ не принимается, но ничего не
/// падает. Асинхронная — для приложения, блокирующая — для bootloader'а: тот
/// с ней не обменивает разделы, а загружает текущий образ.
#[derive(Debug, Default)]
pub struct FlashBus;

/// Ответ [`FlashBus`] на любую команду.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub struct NotConnected;

impl w25q::bus::FlashBusAsync for FlashBus {
    type Error = NotConnected;

    /// Ширина настоящей шины: по ней драйвер выбирает команды{% if ext_flash_bus != "spi" %} (и ставит
    /// бит QE){% endif %}.
    const LANES: w25q::bus::Lanes = w25q::bus::Lanes::{% if ext_flash_bus == "spi" %}Single{% else %}Quad{% endif %};

    fn transfer(
        &mut self,
        _: &w25q::bus::Command,
        _: w25q::bus::Data<'_>,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Err(NotConnected))
    }
}

impl w25q::bus::FlashBusSync for FlashBus {
    type Error = NotConnected;

    const LANES: w25q::bus::Lanes = <Self as w25q::bus::FlashBusAsync>::LANES;

    fn transfer(
        &mut self,
        _: &w25q::bus::Command,
        _: w25q::bus::Data<'_>,
    ) -> Result<(), Self::Error> {
        Err(NotConnected)
    }
}
{%- endif %}
