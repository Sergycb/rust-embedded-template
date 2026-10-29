//! Задачи узлов проекта: по модулю на задачу (или пул задач).
//!
//! В модуле — задача-обёртка `#[embassy_executor::task]` — она раздаёт узлу
//! рычаги платы и адаптеры над ними в памяти своих статиков —
//! и `spawn`. `main` отдаёт плату (`bsp::Board`) в [`spawn_all`] целиком и сам
//! задачами не занимается. Embassy-задача не может быть generic, поэтому
//! каждая типизирована рычагом платы (`bsp`) или типами канала
//! (`crate::link`). Контекст узла собирается из локальных переменных кадра
//! задачи. Сама задача — `task` узла из `domain`.
//!
//! Сторожа и перезапусков нет: узлы не падают, а зависший остаётся зависшим —
//! сторожить исполнитель вам. Сети без графа нет: стек, его узел и узлы
//! служб шаблон ставит только графом (`graph=yes`).
{%- assign xs = links | split: "," %}
{%- if ota == "true" %}
pub mod ota;
// Остальные модули приватные: их задачи спавнит только `spawn_all`.
{%- endif %}
{%- if has_canopen == "true" %}
mod canopen;
{%- endif %}
{%- if has_modbus == "true" %}
mod modbus;
{%- endif %}
{%- if has_protobuf == "true" %}
mod protobuf;
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
pub fn spawn_all(spawner: embassy_executor::Spawner, {% if ota == "true" or has_usb == "true" or has_modbus == "true" or has_protobuf == "true" or has_canopen == "true" %}board{% else %}_board{% endif %}: bsp::Board) {
    // Загрузку процессора меряет задача из `cpu_load.rs`: в домене
    // executor-обвязки быть не может, а там ровно место для неё.
    spawner.spawn(crate::cpu_load::task().expect("задача CPU_LOAD свежая: спавнится один раз"));
{%- if ota == "true" %}
    ota::spawn(spawner, board.firmware);
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
    usb::{{ f[4] }}::spawn(spawner, board.{{ f[4] }}.device);
{%- endif %}
{%- endfor %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "modbus" %}
    modbus::{{ f[4] }}::spawn(spawner, {% if f[0] == "uart" %}board.{{ f[4] }}{% else %}board.{{ f[4] }}.cdc{% endif %}, crate::modbus::handle());
{%- elsif f[3] == "protobuf" %}
    let info = crate::device_info(board.spec);
    protobuf::{{ f[4] }}::spawn(spawner, {% if f[0] == "uart" %}board.{{ f[4] }}{% else %}board.{{ f[4] }}.cdc{% endif %}, info);
{%- elsif f[3] == "canopen" %}
    canopen::{{ f[4] }}::spawn(
        spawner,
        board.{{ f[4] }},
        {% if config == "true" %}adapters::settings::Settings::whole(board.config){% else %}adapters::settings::Unavailable{% endif %},
        domain::canopen::serial(board.spec.uid),
    );
{%- endif %}
{%- endfor %}
}
