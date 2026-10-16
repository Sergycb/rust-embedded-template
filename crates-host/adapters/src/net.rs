//! Сеть поверх `embassy-net`: заглушка сетевого устройства и транспорты
//! протоколов — TCP-сервер и UDP-канал.
//!
//! Ничего из этого не знает чипа: устройство — любой
//! [`Driver`](embassy_net::driver::Driver), сокеты — стек `embassy-net`.
//! Поэтому здесь, а не в `bsp`: `bsp` только собирает стек над своим
//! Ethernet и отдаёт эти объекты в `Board`.

mod tcp;
mod udp;

use core::task::Context;

use embassy_net::driver::{Capabilities, Driver, HardwareAddress, LinkState, RxToken, TxToken};

pub use tcp::{TcpError, TcpServer};
pub use udp::{UdpError, UdpLink};

/// Сетевое устройство, которого ещё нет на плате, — место под Ethernet.
///
/// Как `serial::Unconnected` в слоте UART, но на уровень ниже: стек
/// `embassy-net`, сокеты и узел `NET` настоящие, а у устройства линк всегда
/// опущен — кадров нет ни на приёме, ни на передаче. Проект с ним
/// собирается и запускается, протокол ждёт соединения вечно. Заменить его —
/// значит в `bsp::net` подставить `Ethernet` своей платы.
#[derive(Debug)]
pub struct Unconnected {
    mac: [u8; 6],
}

impl Unconnected {
    /// Устройство с адресом `mac` — его увидит стек, хотя в сеть ничего не
    /// уходит.
    #[must_use]
    pub const fn new(mac: [u8; 6]) -> Self {
        Self { mac }
    }
}

/// Кадр, которого не бывает: токены приёма и передачи [`Unconnected`].
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

/// MTU Ethernet: 1500 байт IP и 14 байт заголовка.
const ETHERNET_MTU: usize = 1514;

impl Driver for Unconnected {
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
    fn unconnected_device_has_no_link_and_no_frames() {
        let mut device = Unconnected::new([2, 1, 2, 3, 4, 5]);
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

#[cfg(test)]
mod fakes {
    //! Кабель в памяти между двумя стеками `embassy-net` — общий для тестов
    //! `net::tcp` и `net::udp`.

    use core::task::{Context, Waker};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use embassy_net::driver::{Capabilities, Driver, HardwareAddress, LinkState, RxToken, TxToken};
    use embassy_net::{
        Config, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4,
    };

    use crate::net::ETHERNET_MTU;

    /// Половина кабеля: кадры, ждущие приёма, и кто их ждёт.
    #[derive(Default)]
    pub(super) struct Wire {
        frames: VecDeque<Vec<u8>>,
        waker: Option<Waker>,
    }

    /// Устройство на одном конце кабеля: принимает из `inbox`, отправляет в
    /// `outbox` — второй конец читает их как свой `inbox`.
    pub(super) struct Loopback {
        inbox: Rc<RefCell<Wire>>,
        outbox: Rc<RefCell<Wire>>,
        mac: [u8; 6],
    }

    pub(super) struct Rx(Vec<u8>);
    pub(super) struct Tx(Rc<RefCell<Wire>>);

    impl RxToken for Rx {
        fn consume<R, F: FnOnce(&mut [u8]) -> R>(mut self, f: F) -> R {
            f(&mut self.0)
        }
    }

    impl TxToken for Tx {
        fn consume<R, F: FnOnce(&mut [u8]) -> R>(self, len: usize, f: F) -> R {
            let mut frame = vec![0; len];
            let result = f(&mut frame);
            let mut wire = self.0.borrow_mut();
            wire.frames.push_back(frame);
            if let Some(waker) = wire.waker.take() {
                waker.wake();
            }
            result
        }
    }

    impl Driver for Loopback {
        type RxToken<'a> = Rx;
        type TxToken<'a> = Tx;

        fn receive(&mut self, cx: &mut Context) -> Option<(Rx, Tx)> {
            let mut inbox = self.inbox.borrow_mut();
            if let Some(frame) = inbox.frames.pop_front() {
                Some((Rx(frame), Tx(Rc::clone(&self.outbox))))
            } else {
                inbox.waker = Some(cx.waker().clone());
                None
            }
        }

        fn transmit(&mut self, _: &mut Context) -> Option<Tx> {
            Some(Tx(Rc::clone(&self.outbox)))
        }

        fn link_state(&mut self, _: &mut Context) -> LinkState {
            LinkState::Up
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

    pub(super) type Node = (Stack<'static>, Runner<'static, Loopback>);

    pub(super) const DEVICE: Ipv4Address = Ipv4Address::new(10, 0, 0, 1);
    const HOST: Ipv4Address = Ipv4Address::new(10, 0, 0, 2);
    pub(super) const PORT: u16 = 502;

    pub(super) fn leak<T>(value: T) -> &'static mut T {
        Box::leak(Box::new(value))
    }

    /// Стек на конце кабеля `device` с адресом `address`.
    fn stack(device: Loopback, address: Ipv4Address) -> Node {
        #[expect(
            clippy::default_trait_access,
            reason = "тип списка DNS — heapless из embassy-net, по имени не назван"
        )]
        let config = Config::ipv4_static(StaticConfigV4 {
            address: Ipv4Cidr::new(address, 24),
            gateway: None,
            dns_servers: Default::default(),
        });
        let seed = u64::from(address.octets()[3]);
        embassy_net::new(device, config, leak(StackResources::<4>::new()), seed)
    }

    /// Два стека — устройство и хост — на одном кабеле.
    pub(super) fn pair() -> (Node, Node) {
        let (a, b): (Rc<RefCell<Wire>>, Rc<RefCell<Wire>>) = (Rc::default(), Rc::default());
        let device = Loopback {
            inbox: Rc::clone(&a),
            outbox: Rc::clone(&b),
            mac: [2, 0, 0, 0, 0, 1],
        };
        let host = Loopback {
            inbox: b,
            outbox: a,
            mac: [2, 0, 0, 0, 0, 2],
        };
        (stack(device, DEVICE), stack(host, HOST))
    }

    /// Гоняет оба стека, пока идёт `test`.
    pub(super) async fn with_network<R>(
        mut device: Runner<'static, Loopback>,
        mut host: Runner<'static, Loopback>,
        test: impl Future<Output = R>,
    ) -> R {
        tokio::select! {
            never = device.run() => never,
            never = host.run() => never,
            result = test => result,
        }
    }
}
