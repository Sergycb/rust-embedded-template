//! Узлы `PROTOBUF_*`: сообщения protobuf от хоста, по модулю на транспорт.
//! Канал узел собирает сам над рычагом платы; тип и буферы — `crate::link`.
{%- assign xs = links | split: "," %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, как в графе (`crate::graph`). {% endcomment %}
{%- assign client = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- assign lm = "crate::link::" | append: f[0] %}
{%- comment %} Без графа транспорт — UART или USB: у них модуль канала на транспорт. {% endcomment %}
{%- assign lm = lm | append: "::" | append: f[4] %}
{%- if f[3] == "protobuf" %}

/// Узел `PROTOBUF_{{ f[4] | upcase }}`.
pub mod {{ f[4] }} {
    use domain::Assemble as _;
    use domain::protobuf::DeviceInfo;
    use embassy_executor::Spawner;

    // Импорт, а не полный путь в сигнатурах: длина строки не зависит от
    // имени транспорта, и rustfmt не переносит её ни при каком имени.
    use {{ lm }}::{Link, Source, buffers, response};

    #[embassy_executor::task]
    async fn task(source: Source, info: DeviceInfo) {
        // Канал — над рычагом платы.
        let mut link = Link::assemble(source, buffers());
        let mut service = domain::protobuf::Service::new(
            info,
{%- if ota == "true" %}
            domain::ota::OtaClient::new(
                crate::tasks::ota::CMD.requester(),
                domain::ota::ClientId({{ client }}),
            ),
{%- endif %}
        );
        domain::protobuf::task(domain::protobuf::ProtobufCtx {
            link: &mut link,
            service: &mut service,
            response: response(),
            index: 0,
        })
        .await;
    }

    /// Спавнит узел над рычагом платы `source`; канал собирает узел.
    pub fn spawn(spawner: Spawner, source: Source, info: DeviceInfo) {
        let token = task(source, info);
        spawner.spawn(token.expect("задача PROTOBUF_{{ f[4] | upcase }} свежая: спавнится один раз"));
    }
}
{%- endif %}
{%- if f[3] != "none" %}{% assign client = client | plus: 1 %}{% endif %}
{%- endfor %}
