//! Узлы `HOST_*`: сообщения protobuf от хоста, по модулю на транспорт.
//! Канал узел собирает сам над рычагом платы; тип и буферы — `crate::link`.
{%- assign xs = links | split: "," %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, как в графе (`crate::graph`). {% endcomment %}
{%- assign client = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- assign lm = "crate::link::" | append: f[0] %}
{%- if f[0] == "uart" or f[0] == "usb" %}
{%- assign lm = lm | append: "::" | append: f[4] %}
{%- endif %}
{%- if f[3] == "protobuf" %}

/// Узел `HOST_{{ f[4] | upcase }}`.
pub mod {{ f[4] }} {
    use domain::host::DeviceInfo;
    use embassy_executor::Spawner;

    // Импорт, а не полный путь в сигнатурах: длина строки не зависит от
    // имени транспорта, и rustfmt не переносит её ни при каком имени.
    use {{ lm }}::{Link, Source, buffers};

    #[embassy_executor::task]
    async fn task(source: Source, info: DeviceInfo) {
{%- if ota == "true" %}
        let service = domain::host::Service::new(
            info,
            domain::ota::OtaClient::new(
                crate::tasks::ota::CMD.requester(),
                domain::ota::ClientId({{ client }}),
            ),
        );
{%- else %}
        let service = domain::host::Service::new(info);
{%- endif %}
        domain::server::serve::<Link, _>(domain::server::ServerWiring {
            source,
            buffers: buffers(),
            handler: domain::host::Protobuf(service),
        })
        .await;
    }

    /// Спавнит узел над рычагом платы `source`; канал собирает узел.
    pub fn spawn(spawner: Spawner, source: Source, info: DeviceInfo) {
        let token = task(source, info);
        spawner.spawn(token.expect("задача HOST_{{ f[4] | upcase }} свежая: спавнится один раз"));
    }
}
{%- endif %}
{%- if f[3] != "none" %}{% assign client = client | plus: 1 %}{% endif %}
{%- endfor %}
