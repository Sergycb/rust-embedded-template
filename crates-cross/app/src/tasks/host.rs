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
    async fn task(mut link: Link, info: DeviceInfo) {
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
        domain::server::task(domain::server::ServerCtx {
            link: &mut link,
            handler: &mut domain::host::Protobuf(service),
            index: 0,
        })
        .await;
    }

    /// Собирает транспорт над `source` и спавнит узел.
    pub fn spawn(spawner: Spawner, source: Source, info: DeviceInfo) {
        let token = task(crate::link::{{ f[4] }}::new(source), info);
        spawner.spawn(token.expect("задача HOST_{{ f[4] | upcase }} свежая: спавнится один раз"));
    }
}
{%- endif %}
{%- if f[3] != "none" %}{% assign client = client | plus: 1 %}{% endif %}
{%- endfor %}
