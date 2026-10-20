//! Задачи узлов проекта без графа — то, что с графом делает `fragments:` в
//! `graph.rs`.
//!
//! По модулю на задачу (или пул задач): в нём задача-обёртка
//! `#[embassy_executor::task]`, сборка адаптеров узла над объектами платы и
//! `spawn`. `main` отдаёт объекты узлов (`crate::parts::Parts`) в [`spawn_all`] целиком, как графу, и сам
//! задачами не занимается. Embassy-задача не может быть generic, поэтому
//! каждая типизирована объектом платы (`bsp`) или транспортом
//! (`crate::link`). Контекст узла собирается из локальных переменных кадра:
//! то, что в графе держит `static`-слот, без графа живёт в задаче. Сама
//! задача — `task` узла из `domain`, та же, что гоняет граф.
//!
//! Без графа нет ни сторожа, ни `restart:`: узлы не падают, а зависший
//! остаётся зависшим — сторожить исполнитель вам.
{%- assign xs = links | split: "," %}
{%- if has_canopen == "true" %}
mod canopen;
{%- endif %}
{%- if has_protobuf == "true" %}
mod host;
{%- endif %}
{%- if has_modbus == "true" %}
mod modbus;
{%- endif %}
{%- if has_net == "true" %}
mod net;
{%- endif %}
{%- if ota == "true" %}
pub mod ota;
{%- endif %}
{%- if has_usb == "true" %}
mod usb;
{%- endif %}

/// Спавнит задачи всех узлов проекта.
///
/// # Panics
///
/// Если задачу уже спавнили: `spawn_all` зовётся один раз за запуск, а
/// проглоченный `SpawnError::Busy` означал бы, что узел молча не работает.
pub fn spawn_all(spawner: embassy_executor::Spawner, {% if ota == "true" or has_net == "true" or has_usb == "true" or has_modbus == "true" or has_protobuf == "true" or has_canopen == "true" %}parts{% else %}_parts{% endif %}: crate::parts::Parts) {
    // Загрузку процессора меряет задача из `cpu_load.rs`: в домене
    // executor-обвязки быть не может, а там ровно место для неё.
    spawner.spawn(crate::cpu_load::task().expect("задача CPU_LOAD свежая: спавнится один раз"));
{%- if ota == "true" %}
    ota::spawn(spawner, parts.board.ota);
{%- endif %}
{%- if has_net == "true" %}
    net::spawn(spawner, parts.board.net.runner);
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
    usb::{{ f[4] }}::spawn(spawner, parts.{{ f[4] }}.device);
{%- endif %}
{%- endfor %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "modbus" %}
    modbus::{{ f[4] }}::spawn(spawner, {% if f[0] == "uart" %}parts.board.{{ f[4] }}{% elsif f[0] == "usb" %}parts.{{ f[4] }}.cdc{% else %}parts.board.net.stack{% endif %});
{%- elsif f[3] == "protobuf" %}
    let info = crate::device_info(parts.board.spec);
    host::{{ f[4] }}::spawn(spawner, {% if f[0] == "uart" %}parts.board.{{ f[4] }}{% elsif f[0] == "usb" %}parts.{{ f[4] }}.cdc{% else %}parts.board.net.stack{% endif %}, info);
{%- elsif f[3] == "canopen" %}
    canopen::{{ f[4] }}::spawn(spawner, parts.board.{{ f[4] }}, {% if config == "true" %}parts.board.settings{% else %}adapters::settings::Unavailable{% endif %});
{%- endif %}
{%- endfor %}
}
