//! Транспорты протоколов над сокетами `embassy-net`:
// {%- if has_tcp == "true" %}
//! TCP-сервер — поток байтов ([`Server`](crate::net::Server));
// {%- endif %}
// {%- if has_udp == "true" %}
//! UDP-канал — кадр в датаграмме ([`UdpLink`](crate::net::UdpLink));
// {%- endif %}
//! стек строит плата (`bsp::net`), транспорты на нём — compose-site.

// {%- if has_tcp == "true" %}
mod tcp;
// {%- endif %}
// {%- if has_udp == "true" %}
mod udp;
// {%- endif %}

// {%- if has_tcp == "true" %}
pub use tcp::{Error, Server};
// {%- endif %}
// {%- if has_udp == "true" %}
pub use udp::{UdpError, UdpLink};
// {%- endif %}

#[cfg(test)]
pub(crate) mod fake {
    //! Поверх кабеля в памяти (`adapters_hw::net::fake::cable`) — то, что нужно тестам
    //! транспортов: порт и прогон обоих стеков.

    use adapters_hw::net::fake::cable::Loopback;
    use embassy_net::Runner;

    pub(crate) const PORT: u16 = 502;

    /// Гоняет оба стека, пока идёт `test`.
    pub(crate) async fn with_network<R>(
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
