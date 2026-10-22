//! Узлы `MODBUS_*`: Modbus slave, адрес 1, по модулю на транспорт. Транспорт
//! собирает `crate::link` над объектом платы; карта регистров одна на все узлы
//! (`crate::modbus`), каждый получает её ручку.
{%- assign xs = links | split: "," %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, как в графе (`crate::graph`). {% endcomment %}
{%- assign client = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "modbus" %}

/// Узел `MODBUS_{{ f[4] | upcase }}`.
pub mod {{ f[4] }} {
    use embassy_executor::Spawner;

    // Импорт, а не полный путь в сигнатурах: длина строки не зависит от
    // имени транспорта, и rustfmt не переносит её ни при каком имени.
    use crate::link::{{ f[4] }}::{Link, Source};

    #[embassy_executor::task]
    async fn task(mut link: Link, map: crate::modbus::Handle) {
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
        domain::server::task(domain::server::ServerCtx {
            link: &mut link,
            handler: &mut domain::modbus::Slave::new(1, regs),
            index: 0,
        })
        .await;
    }

    /// Собирает транспорт над `source` и спавнит узел над картой `map`.
    pub fn spawn(spawner: Spawner, source: Source, map: crate::modbus::Handle) {
        let link = crate::link::{{ f[4] }}::new(source);
        let token = task(link, map);
        spawner.spawn(token.expect("задача MODBUS_{{ f[4] | upcase }} свежая: спавнится один раз"));
    }
}
{%- endif %}
{%- if f[3] != "none" %}{% assign client = client | plus: 1 %}{% endif %}
{%- endfor %}
