//! Узлы `HOST_*`: сообщения protobuf от хоста, по модулю на транспорт.
//! Транспорт с буфером сообщения заводит `crate::link` над объектом платы.
{%- assign xs = links | split: "," %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, как в графе (`crate::graph`). {% endcomment %}
{%- assign client = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "protobuf" %}

/// Узел `HOST_{{ f[4] | upcase }}`.
pub mod {{ f[4] }} {
    use domain::host::DeviceInfo;
    use embassy_executor::Spawner;

    // Импорт, а не полный путь в сигнатурах: длина строки не зависит от
    // имени транспорта, и rustfmt не переносит её ни при каком имени.
    use crate::link::{{ f[4] }}::{Link, Source};

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
            buffers: crate::link::{{ f[4] }}::buffers(),
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
