//! Сеть поверх `embassy-net`: транспорт протокола —
//! {% if has_tcp == "true" %}TCP-сервер{% else %}UDP-канал{% endif %}{% if net_services contains "mqtt" %}, и TCP-клиент
//! брокера MQTT{% endif %}.
//!
//! Ничего из этого не знает чипа: устройство — любой
//! [`Driver`](embassy_net::driver::Driver), сокеты — стек `embassy-net`.
//! Плата собирает стек над своим устройством (Ethernet) и отдаёт его вместе с
//! фоновой работой (`Net`); транспорты на стеке строит граф и отдаёт узлам.
// {%- if net_services contains "mqtt" %}
//! Клиент брокера — тоже: граф строит [`TcpClient`](crate::net::TcpClient) (порт `ports::Dial`).
// {%- endif %}
// {%- if net_services contains "rest" or net_services contains "ssh" %}
//! Серверы служб (`domain::http`, `domain::ssh`) адаптера не получают —
//! docs/architecture.md, «Адаптеры делятся по одному признаку — нужен ли чип».
// {%- endif %}

// {%- if has_tcp == "true" or net_services contains "mqtt" %}
mod tcp;
// {%- endif %}
// {%- if has_udp == "true" %}
mod udp;
// {%- endif %}

use embassy_net::driver::Driver;

// {%- if has_tcp == "true" or net_services contains "mqtt" %}
pub use tcp::TcpError;
// {%- endif %}
// {%- if net_services contains "mqtt" %}
pub use tcp::TcpClient;
// {%- endif %}
// {%- if has_tcp == "true" %}
pub use tcp::{HangUp, TcpServer};
// {%- endif %}
// {%- if has_udp == "true" %}
pub use udp::{UdpError, UdpLink};
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
    //! Поверх кабеля в памяти (`crate::fake::cable`) — то, что нужно тестам
    //! `net::{% if has_tcp == "true" %}tcp{% else %}udp{% endif %}`: порт и прогон обоих стеков.

    use embassy_net::Runner;

    use crate::fake::cable::Loopback;

    pub(super) const PORT: u16 = 502;

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
