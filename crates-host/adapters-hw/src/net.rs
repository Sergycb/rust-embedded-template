//! Сеть платы поверх `embassy-net`: стек и его фоновая работа.
//!
//! Ничего из этого не знает чипа: устройство — любой
//! [`Driver`](embassy_net::driver::Driver), сокеты — стек `embassy-net`.
//! Плата собирает стек над своим устройством (Ethernet) и отдаёт его вместе с
//! фоновой работой (`Net`); транспорты на стеке строит compose-site.

#![cfg_attr(
    any(test, feature = "fake"),
    expect(
        clippy::arbitrary_source_item_ordering,
        reason = "фейк — в конце модуля, который он подменяет, перед тестами: \
                  docs/conventions.md, «Фейки»; модуль `fake` под `cfg(test)` \
                  линт пропускает сам, под фичей — нет"
    )
)]

pub mod stub {
    //! Заглушка устройства до Ethernet: стек над ним настоящий, а линк
    //! всегда опущен — кадров нет ни на приёме, ни на передаче.

    use core::task::Context;

    use embassy_net::driver::{Capabilities, HardwareAddress, LinkState, RxToken, TxToken};

    /// MTU Ethernet: 1500 байт IP и 14 байт заголовка.
    pub const ETHERNET_MTU: usize = 1514;

    /// Сетевое устройство без линка: кадров нет ни на приёме, ни на передаче.
    #[derive(Debug)]
    pub struct Device {
        mac: [u8; 6],
    }

    impl Device {
        /// Устройство с адресом `mac` — его увидит стек, хотя в сеть ничего не
        /// уходит.
        #[must_use]
        pub const fn new(mac: [u8; 6]) -> Self {
            Self { mac }
        }
    }

    /// Кадр, которого не бывает: токены приёма и передачи [`Device`].
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

    impl embassy_net::driver::Driver for Device {
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
}

use embassy_net::driver::Driver;

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

/// Кабель в памяти ([`fake::cable`]), по которому кадры между двумя стеками
/// доходят, — для host-тестов транспортов и сервисов.
#[cfg(any(test, feature = "fake"))]
pub mod fake {
    /// Кабель в памяти между двумя стеками `embassy-net` — устройством и хостом:
    /// кадры доходят, время стоит. Для тестов транспортов `adapters_logic::net`
    // {%- if net_services contains "rest" %}
    /// и сервера REST (`domain::http`)
    // {%- endif %}
    /// — стек, сокеты и сервер над ними настоящие, линк — очередь кадров.
    ///
    /// Только на хосте: ему нужен `alloc`, а у прошивки глобального
    /// аллокатора нет.
    #[cfg(not(target_os = "none"))]
    pub mod cable {
        extern crate alloc;

        use alloc::boxed::Box;
        use alloc::collections::VecDeque;
        use alloc::rc::Rc;
        use alloc::vec;
        use alloc::vec::Vec;
        use core::cell::RefCell;
        use core::task::{Context, Waker};

        use embassy_net::driver::{
            Capabilities, Driver, HardwareAddress, LinkState, RxToken, TxToken,
        };
        use embassy_net::{
            Config, Ipv4Address, Ipv4Cidr, Runner, Stack, StackResources, StaticConfigV4,
        };

        use crate::net::stub::ETHERNET_MTU;

        /// Адрес стека-устройства.
        pub const DEVICE: Ipv4Address = Ipv4Address::new(10, 0, 0, 1);
        /// Адрес стека-хоста.
        pub const HOST: Ipv4Address = Ipv4Address::new(10, 0, 0, 2);

        /// Стек на одном конце кабеля и его фоновая работа.
        pub type Node = (Stack<'static>, Runner<'static, Loopback>);

        /// Половина кабеля: кадры, ждущие приёма, и кто их ждёт.
        #[derive(Debug, Default)]
        struct Wire {
            frames: VecDeque<Vec<u8>>,
            waker: Option<Waker>,
        }

        /// Устройство на одном конце кабеля: принимает из `inbox`, отправляет в
        /// `outbox` — второй конец читает их как свой `inbox`.
        #[derive(Debug)]
        pub struct Loopback {
            inbox: Rc<RefCell<Wire>>,
            outbox: Rc<RefCell<Wire>>,
            mac: [u8; 6],
        }

        /// Принятый кадр.
        #[derive(Debug)]
        pub struct Rx(Vec<u8>);

        /// Место под кадр на передачу — в `outbox` своего конца.
        #[derive(Debug)]
        pub struct Tx(Rc<RefCell<Wire>>);

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

        /// `value` на всё время теста — как `static` в прошивке.
        pub fn leak<T>(value: T) -> &'static mut T {
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

        /// Два стека — устройство ([`DEVICE`]) и хост ([`HOST`]) — на одном кабеле.
        #[must_use]
        pub fn pair() -> (Node, Node) {
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
    }
}

#[cfg(test)]
mod tests {
    use core::task::{Context, Waker};

    use embassy_net::driver::{Driver, HardwareAddress, LinkState};

    use super::stub::{Device, ETHERNET_MTU};

    #[test]
    fn net_device_has_no_link_and_no_frames() {
        let mut device = Device::new([2, 1, 2, 3, 4, 5]);
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
