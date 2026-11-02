//! Задачи узлов проекта: по модулю на задачу (или пул задач).
//!
//! В модуле — задача-обёртка `#[embassy_executor::task]` — она раздаёт узлу
//! рычаги платы и адаптеры над ними в памяти своих статиков —
//! и `spawn`. Задача `start` бинарника отдаёт плату (`bsp::Board`) в
//! [`spawn_all`] целиком и сама задачами узлов не занимается. Embassy-задача
//! не может быть generic, поэтому каждая типизирована рычагом платы (`bsp`);
//! канал протокола над ним задача строит сама, конкретным типом. Контекст узла
//! собирается из локальных переменных кадра задачи. Сама задача — `task` узла из `domain`.
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

/// Спавнит задачи всех узлов проекта{% if ota == "true" %}; `image` — свойства образа, которые
/// отдаёт задача `start` бинарника (`crate::version`){% endif %}.
///
/// # Panics
///
/// Если задачу уже спавнили: `spawn_all` зовётся один раз за запуск, а
/// проглоченный `SpawnError::Busy` означал бы, что узел молча не работает.
{%- if ota == "true" %}
pub fn spawn_all(
    spawner: embassy_executor::Spawner,
    board: bsp::Board,
    image: crate::version::Image,
) {
{%- else %}
pub fn spawn_all(spawner: embassy_executor::Spawner, {% if has_usb == "true" or has_modbus == "true" or has_protobuf == "true" or has_canopen == "true" %}board{% else %}_board{% endif %}: bsp::Board) {
{%- endif %}
    // Загрузку процессора меряет задача из `cpu_load.rs`: в домене
    // executor-обвязки быть не может, а там ровно место для неё.
    spawner.spawn(crate::cpu_load::task().expect("задача CPU_LOAD свежая: спавнится один раз"));
{%- if ota == "true" %}
    ota::spawn(spawner, board.firmware, image);
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
    modbus::{{ f[4] }}::spawn(spawner, {% if f[0] == "uart" %}board.{{ f[4] }}{% else %}board.{{ f[4] }}.cdc{% endif %}, crate::modbus::handle(){% if ota == "true" %}, image.text{% endif %});
{%- elsif f[3] == "protobuf" %}
    let info = crate::device::info(board.spec{% if ota == "true" %}, image{% endif %});
    protobuf::{{ f[4] }}::spawn(spawner, {% if f[0] == "uart" %}board.{{ f[4] }}{% else %}board.{{ f[4] }}.cdc{% endif %}, info);
{%- elsif f[3] == "canopen" %}
    canopen::{{ f[4] }}::spawn(
        spawner,
        board.{{ f[4] }},
        {% if config == "true" %}Some(domain::settings::Settings::whole(board.config)){% else %}None{% endif %},
        domain::canopen::serial(board.spec.uid),
    );
{%- endif %}
{%- endfor %}
}
