//! Узлы `MODBUS_*`: Modbus slave, адрес 1, по модулю на транспорт. Канал —
//! кадр RTU над рычагом платы, конкретным типом прямо в задаче; карта
//! регистров одна на все узлы (`crate::registers`), каждый получает её ручку.
{%- assign xs = links | split: "," %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, как в графе (`crate::graph`). {% endcomment %}
{%- assign client = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "modbus" %}

/// Узел `MODBUS_{{ f[4] | upcase }}`.
pub mod {{ f[4] }} {
    use embassy_executor::Spawner;

    /// Рычаг платы, над которым строится канал (`board.{{ f[4] }}{% if f[0] == "usb" %}.cdc{% endif %}`).
    type Source = {% if f[0] == "usb" %}bsp::usb::{{ f[4] }}::Cdc{% else %}bsp::serial::{{ f[4] }}::Serial{% endif %};

    #[embassy_executor::task]
    async fn task(source: Source, map: crate::registers::Handle{% if ota == "true" %}, version: &'static str{% endif %}) {
{%- if ota == "true" %}
        let regs = domain::modbus::OtaRegisters::new(
            map,
            domain::ota::Client::new(crate::tasks::ota::CMD.requester(), domain::ota::ClientId({{ client }})),
        );
{%- else %}
        let regs = map;
{%- endif %}
        // Кадр RTU памяти извне не просит.
        let mut link = adapters_logic::modbus::Rtu::new(source);
        let mut handler = domain::modbus::Slave::new(1, regs, {% if ota == "true" %}version{% else %}crate::build::PKG_VERSION{% endif %});
        domain::modbus::task(domain::modbus::ModbusCtx {
            link: &mut link,
            handler: &mut handler,
            index: 0,
        })
        .await;
    }
{%- if ota == "true" %}

    /// Спавнит узел над рычагом платы `source` и картой `map`; `version` —
    /// версия образа текстом (свойства образа бинарника), которую slave
    /// называет хосту.
    pub fn spawn(
        spawner: Spawner,
        source: Source,
        map: crate::registers::Handle,
        version: &'static str,
    ) {
        let token = task(source, map, version);
{%- else %}

    /// Спавнит узел над рычагом платы `source` и картой `map`.
    pub fn spawn(spawner: Spawner, source: Source, map: crate::registers::Handle) {
        let token = task(source, map);
{%- endif %}
        spawner.spawn(token.expect("задача MODBUS_{{ f[4] | upcase }} свежая: спавнится один раз"));
    }
}
{%- endif %}
{%- if f[3] != "none" %}{% assign client = client | plus: 1 %}{% endif %}
{%- endfor %}
