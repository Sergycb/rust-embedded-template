//! Объекты узлов: плата и то, что приложение собрало над её драйверами.
//!
//! `bsp::Board::new` отдаёт два объекта: плату (`bsp::Board`) — её поля
//! уходят узлам как есть — и драйверы (`bsp::Drivers`), над которыми стек
//! собирает приложение, потому что из одного конструктора выходят объекты для
//! разных узлов{% if has_usb == "true" %}: USB-устройство (узел `USB_*`) и поток CDC (транспорт
//! протокола) рождаются одним `Builder`{% endif %}. Узлы получают их отдельно, а граф
//! входы узлов собирает отдельными выражениями, поэтому пара собирается здесь,
//! один раз, и в графе делится по полям (`parts.<транспорт>.device`,
//! `parts.<транспорт>.cdc`). Граф берёт [`Parts`] `boot:`-объектом, без
//! графа — `tasks::spawn_all`.
{%- assign xs = links | split: "," %}

/// Плата и собранные над её драйверами пары объектов.
#[derive(Debug)]
pub struct Parts {
    /// Объекты платы.
    pub board: bsp::Board,
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
    /// USB `{{ f[2] }}`: устройство для узла `USB_{{ f[4] | upcase }}` и поток CDC ({{ f[3] }}).
    pub {{ f[4] }}: crate::usb::{{ f[4] }}::Usb,
{%- endif %}
{%- endfor %}
}

impl Parts {
    /// Собирает пары над драйверами платы.
    #[must_use]
{%- if has_usb == "true" %}
    pub fn new((board, drivers): (bsp::Board, bsp::Drivers)) -> Self {
        Self {
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
            {{ f[4] }}: crate::usb::{{ f[4] }}::new(drivers.{{ f[4] }}, board.spec.uid_hex),
{%- endif %}
{%- endfor %}
            board,
        }
    }
{%- else %}
    pub fn new((board, _drivers): (bsp::Board, bsp::Drivers)) -> Self {
        Self { board }
    }
{%- endif %}
}
