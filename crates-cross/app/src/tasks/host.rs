//! Узлы `HOST_*`: сообщения protobuf от хоста, по модулю на транспорт.
//! Транспорт и буферы сообщений заводит `crate::link` над объектом платы.
{%- assign xs = links | split: "," %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, как в графе (`crate::graph`). {% endcomment %}
{%- assign client = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "protobuf" %}

/// Узел `HOST_{{ f[4] | upcase }}`.
pub mod {{ f[4] }} {
    use embassy_executor::Spawner;

    #[embassy_executor::task]
    async fn task(
        mut link: crate::link::{{ f[4] }}::Link,
        info: domain::host::DeviceInfo,
        rx: &'static mut [u8],
        tx: &'static mut [u8],
    ) {
{%- if ota == "true" %}
        let mut service = domain::host::Service::new(
            info,
            domain::ota::OtaClient::new(
                crate::tasks::ota::CMD.requester(),
                domain::ota::ClientId({{ client }}),
            ),
        );
{%- else %}
        let mut service = domain::host::Service::new(info);
{%- endif %}
        domain::host::task(domain::host::HostCtx {
            link: &mut link,
            service: &mut service,
            rx,
            tx,
            index: 0,
        })
        .await;
    }

    /// Собирает транспорт над `source` и спавнит узел.
    pub fn spawn(
        spawner: Spawner,
        source: crate::link::{{ f[4] }}::Source,
        info: domain::host::DeviceInfo,
    ) {
        let link = crate::link::{{ f[4] }}::new(source);
        let rx = crate::link::{{ f[4] }}::rx();
        let tx = crate::link::{{ f[4] }}::tx();
        let token = task(link, info, rx, tx);
        spawner.spawn(token.expect("задача HOST_{{ f[4] | upcase }} свежая: спавнится один раз"));
    }
}
{%- endif %}
{%- if f[3] != "none" %}{% assign client = client | plus: 1 %}{% endif %}
{%- endfor %}
