#![doc = include_str!("../../../docs/modules/cpu-com.md")]
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "uart" %}
{%- assign parts = f[4] | split: "_" %}

/// UART `{{ f[2] }}` чипа — транспорт `{{ f[4] }}`.
pub mod {{ f[4] }} {
    /// UART, который ещё не подключили, — заглушка до `BufferedUart` чипа.
    /// Поток байтов, как у `BufferedUart`: читает вечно, пишет в никуда.
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

    /// Собирает `{{ f[2] }}`. Блок приходит группой `wiring.rs` — заглушка его не
    /// трогает, драйвер чипа соберётся из него и выводов.
    pub(crate) fn new(_resources: crate::wiring::{% for part in parts %}{{ part | capitalize }}{% endfor %}Resources) -> Serial {
        defmt::warn!("cpu: {{ f[2] }} не подключён — {% if f[3] == "none" %}поток ждёт впустую{% else %}узел протокола ждёт впустую{% endif %}");
        Serial
    }
}
{%- endif %}
{%- endfor %}
