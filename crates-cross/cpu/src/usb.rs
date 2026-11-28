//! Драйвер USB-блока чипа — заглушка до USB-блока платы.
//!
//! Устройство `embassy-usb` и класс CDC над драйвером собирает `bsp`
//! (`crates-cross/bsp/src/usb.rs`): от чипа они не зависят, и драйвер —
//! единственное, что `cpu` сюда отдаёт.

/// USB-драйвер — заглушка до USB-блока чипа.
pub type Usb = adapters_hw::usb::stub::UsbDriver;
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
{%- assign parts = f[4] | split: "_" %}

/// Собирает драйвер USB `{{ f[2] }}`. Блок приходит группой `wiring.rs` —
/// заглушка его не трогает, драйвер чипа соберётся из него и выводов.
pub(crate) fn new_{{ f[4] }}(_resources: crate::wiring::{% for part in parts %}{{ part | capitalize }}{% endfor %}Resources) -> Usb {
    defmt::warn!("cpu: {{ f[2] }} не подключён — хост устройства не увидит");
    Usb::default()
}
{%- endif %}
{%- endfor %}
