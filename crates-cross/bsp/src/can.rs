#![doc = include_str!("../../../docs/modules/bsp-can.md")]
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "can" %}
{%- assign parts = f[4] | split: "_" %}

/// Шина CAN `{{ f[2] }}` платы — транспорт `{{ f[4] }}`.
{%- if f[4] == "can" %}
#[expect(
    clippy::module_inception,
    reason = "имя транспорта — имя блока `CAN`, а модуль драйвера — `can`"
)]
{%- endif %}
pub mod {{ f[4] }} {
    /// Шина `{{ f[2] }}` — пока заглушка, до контроллера (см. модуль `bsp::can`).
    pub type Can = adapters::can::Unconnected<embassy_stm32::can::Frame>;

    /// Собирает шину `{{ f[2] }}`. Блок приходит группой `resources.rs` — заглушка
    /// его не трогает, контроллер платы соберётся из него и выводов.
    pub(crate) fn new(_resources: crate::resources::{% for part in parts %}{{ part | capitalize }}{% endfor %}Resources) -> Can {
        defmt::warn!("bsp: {{ f[2] }} не подключён — {% if f[3] == "canopen" %}узел CANopen ждёт впустую{% else %}шина ждёт впустую{% endif %}");
        Can::default()
    }
}
{%- endif %}
{%- endfor %}
