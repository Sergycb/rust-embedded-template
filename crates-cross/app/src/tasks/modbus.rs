//! Узлы `MODBUS_*`: Modbus slave, адрес 1, по модулю на транспорт. Канал узел
//! собирает сам над рычагом платы (тип и буферы — `crate::link`); карта
//! регистров одна на все узлы (`crate::modbus`), каждый получает её ручку.
{%- assign xs = links | split: "," %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, как в графе (`crate::graph`). {% endcomment %}
{%- assign client = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- assign lm = "crate::link::" | append: f[0] %}
{%- comment %} Без графа транспорт — UART или USB: у них модуль канала на транспорт. {% endcomment %}
{%- assign lm = lm | append: "::" | append: f[4] %}
{%- if f[3] == "modbus" %}

/// Узел `MODBUS_{{ f[4] | upcase }}`.
pub mod {{ f[4] }} {
    use embassy_executor::Spawner;

    // Импорт, а не полный путь в сигнатурах: длина строки не зависит от
    // имени транспорта, и rustfmt не переносит её ни при каком имени.
    use {{ lm }}::{Link, Source, buffers};

    #[embassy_executor::task]
    async fn task(source: Source, map: crate::modbus::Handle) {
{%- if ota == "true" %}
        let regs = domain::modbus::OtaRegisters::new(
            map,
            domain::ota::OtaClient::new(
                crate::tasks::ota::CMD.requester(),
                domain::ota::ClientId({{ client }}),
            ),
        );
{%- else %}
        let regs = map;
{%- endif %}
        domain::server::serve::<Link, _>(domain::server::ServerWiring {
            source,
            buffers: buffers(),
            handler: domain::modbus::Slave::new(1, regs),
        })
        .await;
    }

    /// Спавнит узел над рычагом платы `source` и картой `map`; канал
    /// собирает узел.
    pub fn spawn(spawner: Spawner, source: Source, map: crate::modbus::Handle) {
        let token = task(source, map);
        spawner.spawn(token.expect("задача MODBUS_{{ f[4] | upcase }} свежая: спавнится один раз"));
    }
}
{%- endif %}
{%- if f[3] != "none" %}{% assign client = client | plus: 1 %}{% endif %}
{%- endfor %}
