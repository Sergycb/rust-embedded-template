//! Тип хранилища настроек — `adapters::settings` над носителем платы
//! (`bsp::settings::ConfigFlash`): пары «ключ — значение», переживающие
//! перезапуск и обновление прошивки. Формат значений выбирает проект.
{%- if has_canopen != "true" and ssh_host_key != "config" %}
#![expect(
    dead_code,
    reason = "хранилище ждёт потребителя: в шаблоне его берут только узлы CANopen и SSH (ключ хоста), здесь — ваш узел"
)]
{%- endif %}

/// Хранилище над носителем платы. Псевдоним: тип параметра фрагмента и
/// задачи узла должен быть конкретным. Собирает его узел-потребитель (порт
/// `Assemble`) над `board.settings`.
pub type Settings = adapters::settings::Settings<bsp::settings::ConfigFlash>;
