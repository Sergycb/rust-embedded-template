//! Задачи узлов проекта: по модулю на задачу (или пул задач).
//!
//! В модуле — задача-обёртка `#[embassy_executor::task]` — она раздаёт узлу
//! рычаги платы и статики, а адаптеры собирает сам узел (`domain::Assemble`), —
//! и `spawn`. `main` отдаёт плату (`bsp::Board`) в [`spawn_all`] целиком и сам
//! задачами не занимается. Embassy-задача не может быть generic, поэтому
//! каждая типизирована рычагом платы (`bsp`) или типами канала
//! (`crate::link`). Контекст узла собирается из локальных переменных кадра
//! задачи. Сама задача — `task` узла из `domain`.
//!
//! Сторожа и перезапусков нет: узлы не падают, а зависший остаётся зависшим —
//! сторожить исполнитель вам.
{%- assign xs = links | split: "," %}
{%- if ota == "true" %}
pub mod ota;
// Остальные модули приватные: их задачи спавнит только `spawn_all`.
{%- endif %}
{%- if has_canopen == "true" %}
mod canopen;
{%- endif %}
{%- if net_services contains "rest" %}
mod http;
{%- endif %}
{%- if has_modbus == "true" %}
mod modbus;
{%- endif %}
{%- if net_services contains "mqtt" %}
mod mqtt;
{%- endif %}
{%- if has_net == "true" %}
mod net;
{%- endif %}
{%- if has_protobuf == "true" %}
mod protobuf;
{%- endif %}
{%- if net_services contains "ssh" %}
mod ssh;
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
pub fn spawn_all(spawner: embassy_executor::Spawner, {% if ota == "true" or has_net == "true" or has_usb == "true" or has_modbus == "true" or has_protobuf == "true" or has_canopen == "true" %}board{% else %}_board{% endif %}: bsp::Board) {
    // Загрузку процессора меряет задача из `cpu_load.rs`: в домене
    // executor-обвязки быть не может, а там ровно место для неё.
    spawner.spawn(crate::cpu_load::task().expect("задача CPU_LOAD свежая: спавнится один раз"));
{%- if ota == "true" %}
    ota::spawn(spawner, board.firmware);
{%- endif %}
{%- if has_net == "true" %}
    net::spawn(spawner, board.net.runner);
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
    modbus::{{ f[4] }}::spawn(spawner, {% if f[0] == "uart" %}board.{{ f[4] }}{% elsif f[0] == "usb" %}board.{{ f[4] }}.cdc{% else %}board.net.stack{% endif %}, crate::modbus::handle());
{%- elsif f[3] == "protobuf" %}
    let info = crate::device_info(board.spec);
    protobuf::{{ f[4] }}::spawn(spawner, {% if f[0] == "uart" %}board.{{ f[4] }}{% elsif f[0] == "usb" %}board.{{ f[4] }}.cdc{% else %}board.net.stack{% endif %}, info);
{%- elsif f[3] == "canopen" %}
    canopen::{{ f[4] }}::spawn(
        spawner,
        board.{{ f[4] }},
        {% if config == "true" %}board.config{% else %}(){% endif %},
        domain::canopen::serial(board.spec.uid),
    );
{%- endif %}
{%- endfor %}
{%- if net_services contains "rest" %}
    http::spawn(spawner, board.net.stack, crate::device_info(board.spec));
{%- endif %}
{%- if net_services contains "ssh" %}
    ssh::spawn(
        spawner,
        board.net.stack,
        crate::device_info(board.spec),
{%- if ssh_host_key == "config" %}
        board.config,
{%- else %}
        &crate::net::ssh::HOST_KEY,
{%- endif %}
    );
{%- endif %}
{%- if net_services contains "mqtt" %}
    mqtt::spawn(spawner, board.net.stack, crate::device_info(board.spec));
{%- endif %}
}
