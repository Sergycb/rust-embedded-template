//! Узлы `MODBUS_*`: Modbus slave с пустой картой регистров, адрес 1, по
//! модулю на транспорт. Транспорт собирает `crate::link` над объектом платы.
{%- assign xs = links | split: "," %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, как в графе (`crate::graph`). {% endcomment %}
{%- assign client = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "modbus" %}

/// Узел `MODBUS_{{ f[4] | upcase }}`.
pub mod {{ f[4] }} {
    use embassy_executor::Spawner;

    #[embassy_executor::task]
    async fn task(mut link: crate::link::{{ f[4] }}::Link) {
{%- if ota == "true" %}
        let mut regs = domain::modbus::OtaRegisters::new(
            domain::modbus::EmptyRegisters,
            domain::ota::OtaClient::new(
                crate::tasks::ota::CMD.requester(),
                domain::ota::ClientId({{ client }}),
            ),
        );
{%- else %}
        let mut regs = domain::modbus::EmptyRegisters;
{%- endif %}
        let mut unit = 1;
        domain::modbus::task(domain::modbus::ModbusSlaveCtx {
            link: &mut link,
            regs: &mut regs,
            unit: &mut unit,
            index: 0,
        })
        .await;
    }

    /// Собирает транспорт над `source` и спавнит узел.
    pub fn spawn(spawner: Spawner, source: crate::link::{{ f[4] }}::Source) {
        let link = crate::link::{{ f[4] }}::new(source);
        let token = task(link);
        spawner.spawn(token.expect("задача MODBUS_{{ f[4] | upcase }} свежая: спавнится один раз"));
    }
}
{%- endif %}
{%- if f[3] != "none" %}{% assign client = client | plus: 1 %}{% endif %}
{%- endfor %}
