#![doc = include_str!("../../../docs/modules/bsp-usb.md")]
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
{%- assign parts = f[4] | split: "_" %}

/// USB `{{ f[2] }}` платы — транспорт `{{ f[4] }}`. Тип — свой у каждого блока:
/// у настоящего драйвера тип зависит от блока.
{%- if f[4] == "usb" %}
#[expect(
    clippy::module_inception,
    reason = "имя транспорта — имя блока `USB`, а модуль драйвера — `usb`"
)]
{%- endif %}
pub mod {{ f[4] }} {
    /// USB-драйвер `{{ f[2] }}` — пока заглушка, до USB-блока чипа (см. модуль `bsp::usb`).
    /// Устройство и классы над ним собирает приложение (`crates-cross/app/src/usb.rs`).
    pub type UsbDriver = crate::stub::UsbDriver;

    /// Собирает драйвер `{{ f[2] }}`. Блок приходит группой `resources.rs` — заглушка
    /// его не трогает, драйвер платы соберётся из него и выводов.
    pub(crate) fn new(_resources: crate::resources::{% for part in parts %}{{ part | capitalize }}{% endfor %}Resources) -> UsbDriver {
        defmt::warn!("bsp: {{ f[2] }} не подключён — хост устройства не увидит");
        UsbDriver::default()
    }
}
{%- endif %}
{%- endfor %}
