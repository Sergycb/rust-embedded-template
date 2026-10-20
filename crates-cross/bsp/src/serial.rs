#![doc = include_str!("../../../docs/modules/bsp-serial.md")]
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "uart" %}
{%- assign parts = f[4] | split: "_" %}

/// UART `{{ f[2] }}` платы — транспорт `{{ f[4] }}`.
pub mod {{ f[4] }} {
    /// Поток байтов `{{ f[2] }}` — пока заглушка, до драйвера (см. модуль `bsp::serial`).
    pub type Serial = adapters::serial::Unconnected;

    /// Собирает `{{ f[2] }}`. Блок приходит группой `resources.rs` — заглушка его не
    /// трогает, драйвер платы соберётся из него и выводов.
    pub(crate) fn new(_resources: crate::resources::{% for part in parts %}{{ part | capitalize }}{% endfor %}Resources) -> Serial {
        defmt::warn!("bsp: {{ f[2] }} не подключён — {% if f[3] == "none" %}поток ждёт впустую{% else %}узел протокола ждёт впустую{% endif %}");
        adapters::serial::Unconnected
    }
}
{%- endif %}
{%- endfor %}
