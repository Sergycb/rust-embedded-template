//! Драйверы, на которых ничего не происходит, — для host-тестов узлов и
//! транспортов: устройство `embassy-usb` и стек `embassy-net` над ними
//! настоящие, а шина и линк молчат.
//!
//! В прошивку не попадают: в слотах драйверов платы стоят свои заглушки
//! (`bsp::stub`), а сюда ходят тесты `adapters` и `domain` (фича `fake`).
// {%- if has_usb == "true" %}

use embassy_usb::driver::{
    Bus, ControlPipe, Direction, Driver, Endpoint, EndpointAddress, EndpointAllocError,
    EndpointError, EndpointIn, EndpointInfo, EndpointOut, EndpointType, Event, Unsupported,
};
// {%- endif %}
// {%- if has_net == "true" %}

use core::task::Context;

use embassy_net::driver::{Capabilities, HardwareAddress, LinkState, RxToken, TxToken};
// {%- endif %}
// {%- if has_usb == "true" %}

/// USB-драйвер без шины: хост устройство не видит.
#[derive(Debug, Default)]
pub struct UsbDriver {
    /// Сколько конечных точек уже выдано — для их адресов.
    endpoints: usize,
}

/// Конечная точка, по которой ничего не приходит и не уходит.
#[derive(Debug)]
pub struct NoEndpoint(EndpointInfo);

/// Шина и управляющий канал, на которых не происходит ничего.
#[derive(Debug)]
pub struct NoBus;

/// Пакет управляющей точки по умолчанию — 64 байта (full-speed).
const CONTROL_PACKET: usize = 64;

impl UsbDriver {
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

impl Driver<'_> for UsbDriver {
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

#[cfg(test)]
mod usb_tests {
    use core::pin::pin;
    use core::task::{Context, Waker};

    use super::*;

    /// Драйвер выдаёт точки с растущими адресами и ничего не делает: шина и
    /// точки ждут вечно.
    #[test]
    fn usb_driver_never_wakes() {
        let mut driver = UsbDriver::default();
        let out = driver
            .alloc_endpoint_out(EndpointType::Bulk, None, 64, 0)
            .unwrap();
        let mut input = driver
            .alloc_endpoint_in(EndpointType::Bulk, None, 64, 0)
            .unwrap();
        assert_eq!(
            out.info().addr,
            EndpointAddress::from_parts(1, Direction::Out)
        );
        assert_eq!(
            input.info().addr,
            EndpointAddress::from_parts(2, Direction::In)
        );

        let (mut bus, mut control) = driver.start(64);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(pin!(bus.poll()).poll(&mut cx).is_pending());
        assert!(pin!(control.setup()).poll(&mut cx).is_pending());
        assert!(pin!(input.write(&[1])).poll(&mut cx).is_pending());
        assert!(pin!(input.wait_enabled()).poll(&mut cx).is_pending());
        assert_eq!(control.max_packet_size(), CONTROL_PACKET);
        assert!(!bus.endpoint_is_stalled(out.info().addr));
    }
}
// {%- endif %}
// {%- if has_net == "true" %}

/// MTU Ethernet: 1500 байт IP и 14 байт заголовка.
pub const ETHERNET_MTU: usize = 1514;

/// Сетевое устройство без линка: кадров нет ни на приёме, ни на передаче.
#[derive(Debug)]
pub struct NetDevice {
    mac: [u8; 6],
}

impl NetDevice {
    /// Устройство с адресом `mac`.
    #[must_use]
    pub const fn new(mac: [u8; 6]) -> Self {
        Self { mac }
    }
}

/// Кадр, которого не бывает: токены приёма и передачи [`NetDevice`].
#[derive(Debug)]
pub enum NoFrame {}

impl RxToken for NoFrame {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, _: F) -> R {
        match self {}
    }
}

impl TxToken for NoFrame {
    fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, _: usize, _: F) -> R {
        match self {}
    }
}

impl embassy_net::driver::Driver for NetDevice {
    type RxToken<'a> = NoFrame;
    type TxToken<'a> = NoFrame;

    fn receive(&mut self, _: &mut Context) -> Option<(NoFrame, NoFrame)> {
        None
    }

    fn transmit(&mut self, _: &mut Context) -> Option<NoFrame> {
        None
    }

    fn link_state(&mut self, _: &mut Context) -> LinkState {
        LinkState::Down
    }

    fn capabilities(&self) -> Capabilities {
        let mut capabilities = Capabilities::default();
        capabilities.max_transmission_unit = ETHERNET_MTU;
        capabilities
    }

    fn hardware_address(&self) -> HardwareAddress {
        HardwareAddress::Ethernet(self.mac)
    }
}

#[cfg(test)]
mod tests {
    use core::task::{Context, Waker};

    use super::*;

    #[test]
    fn net_device_has_no_link_and_no_frames() {
        use embassy_net::driver::Driver;

        let mut device = NetDevice::new([2, 1, 2, 3, 4, 5]);
        let mut cx = Context::from_waker(Waker::noop());
        assert!(device.receive(&mut cx).is_none());
        assert!(device.transmit(&mut cx).is_none());
        assert!(matches!(device.link_state(&mut cx), LinkState::Down));
        assert_eq!(device.capabilities().max_transmission_unit, ETHERNET_MTU);
        assert_eq!(
            device.hardware_address(),
            HardwareAddress::Ethernet([2, 1, 2, 3, 4, 5])
        );
    }
}
// {%- endif %}
