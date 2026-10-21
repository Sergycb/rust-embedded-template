//! Объекты узлов: плата и то, что приложение собрало над её драйверами.
//!
//! `bsp::Board::new` отдаёт два объекта: плату (`bsp::Board`) — её поля
//! уходят узлам как есть — и драйверы (`bsp::Drivers`), над которыми стек
//! собирает приложение, потому что из одного конструктора выходят объекты для
//! разных узлов{% if has_usb == "true" %}: USB-устройство (узел `USB_*`) и поток CDC (транспорт
//! протокола) рождаются одним `Builder`{% endif %}{% if has_net == "true" %}{% if has_usb == "true" %}, а{% else %}:{% endif %} фоновая работа стека (узел
//! `NET`) и стек для сокетов — одним `embassy_net::new`{% endif %}. Узлы получают их отдельно, а граф
//! входы узлов собирает отдельными выражениями, поэтому пара собирается здесь,
//! один раз, и в графе делится по полям (`parts.<транспорт>.device`,
//! `parts.<транспорт>.cdc`).{% if has_modbus == "true" %} Здесь же один раз заводится карта регистров
//! Modbus (`crate::modbus`): её ручку (`parts.modbus`, `Copy`) получает каждый
//! узел `MODBUS_*`.{% endif %} Граф берёт [`Parts`] `boot:`-объектом, без
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
{%- if has_net == "true" %}
    /// Сеть: фоновая работа стека для узла `NET` и стек для сокетов.
    pub net: crate::net::Net,
{%- endif %}
{%- if has_modbus == "true" %}
    /// Ручка карты регистров Modbus, общей у всех узлов `MODBUS_*`.
    pub modbus: crate::modbus::Handle,
{%- endif %}
}

impl Parts {
    /// Собирает пары над драйверами платы.
    #[must_use]
{%- if has_usb == "true" or has_net == "true" %}
    pub fn new((board, drivers): (bsp::Board, bsp::Drivers)) -> Self {
{%- if has_net == "true" %}
{%- if rng_irq != "" %}
        // Затравка стека — первое слово RNG; блок выключится, когда генератор
        // дропнется в конце сборки.
        let mut rng = drivers.rng;
        let seed = rng.next_u64();
{%- else %}
        let seed = crate::net::seed_from_uid(board.spec.uid);
{%- endif %}
{%- endif %}
        Self {
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
            {{ f[4] }}: crate::usb::{{ f[4] }}::new(drivers.{{ f[4] }}, board.spec.uid_hex),
{%- endif %}
{%- endfor %}
{%- if has_net == "true" %}
            net: crate::net::new(drivers.eth, seed),
{%- endif %}
{%- if has_modbus == "true" %}
            modbus: crate::modbus::new(),
{%- endif %}
            board,
        }
    }
{%- else %}
    pub fn new((board, _drivers): (bsp::Board, bsp::Drivers)) -> Self {
{%- if has_modbus == "true" %}
        Self {
            board,
            modbus: crate::modbus::new(),
        }
{%- else %}
        Self { board }
{%- endif %}
    }
{%- endif %}
}
