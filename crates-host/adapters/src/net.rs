//! Сеть поверх `embassy-net`: транспорт протокола —
//! {% if has_tcp == "true" %}TCP-сервер{% else %}UDP-канал{% endif %}{% if net_services contains "mqtt" %}, и TCP-клиент
//! брокера MQTT{% endif %}.
//!
//! Ничего из этого не знает чипа: устройство — любой
//! [`Driver`](embassy_net::driver::Driver), сокеты — стек `embassy-net`.
//! Плата собирает стек над своим устройством (Ethernet) и отдаёт его вместе с
//! фоновой работой (`Net`); сокеты на стеке собирают узлы (`ports::Assemble`).

// {%- if has_tcp == "true" or net_services contains "mqtt" %}
mod tcp;
// {%- endif %}
// {%- if has_udp == "true" %}
mod udp;
// {%- endif %}

use embassy_net::driver::Driver;

// {%- if has_tcp == "true" or net_services contains "mqtt" %}
pub use tcp::{TcpError, TcpServer, TcpServerBuffers};
// {%- if net_services contains "mqtt" %}
pub use tcp::{TcpClient, TcpClientBuffers};
// {%- endif %}
// {%- endif %}
// {%- if has_udp == "true" %}
pub use udp::{UdpBuffers, UdpError, UdpLink};
// {%- endif %}

/// Сеть платы: фоновая работа стека для узла `NET` и сам стек, на котором
/// узлы открывают свои сокеты.
///
/// Пара, потому что их даёт один `embassy_net::new`; чипа она не знает —
/// устройство `D` подставляет плата.
pub struct Net<D: Driver + 'static> {
    /// Фоновая работа стека — уезжает в узел `NET`.
    pub runner: embassy_net::Runner<'static, D>,
    /// Стек — на нём узлы открывают свои сокеты.
    pub stack: embassy_net::Stack<'static>,
}

// Вручную: у `Runner` нет `Debug`.
impl<D: Driver + 'static> core::fmt::Debug for Net<D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Net").finish_non_exhaustive()
    }
}

#[cfg(test)]
mod fakes {
    //! Кабель в памяти между двумя стеками `embassy-net` — для тестов
    //! `net::{% if has_tcp == "true" %}tcp{% else %}udp{% endif %}`.

    use core::task::{Context, Waker};
    use std::cell::RefCell;
    use std::collections::VecDeque;
    use std::rc::Rc;

    use embassy_net::driver::{Capabilities, Driver, HardwareAddress, LinkState, RxToken, TxToken};
    use embassy_net::{
        Config, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4,
    };

    use crate::fake::ETHERNET_MTU;

    pub(super) const DEVICE: Ipv4Address = Ipv4Address::new(10, 0, 0, 1);
    pub(super) const HOST: Ipv4Address = Ipv4Address::new(10, 0, 0, 2);
    pub(super) const PORT: u16 = 502;

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
