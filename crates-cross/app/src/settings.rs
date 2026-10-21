//! Хранилище настроек приложения — `adapters::settings` над носителем платы
//! (`bsp::settings::ConfigFlash`): пары «ключ — значение», переживающие
//! перезапуск и обновление прошивки. Формат значений выбирает проект.
{%- if has_canopen != "true" and ssh_host_key != "config" %}
#![expect(
    dead_code,
    reason = "хранилище ждёт потребителя: в шаблоне его берут только узлы CANopen и SSH (ключ хоста), здесь — ваш узел"
)]
{%- endif %}

/// Хранилище. Псевдоним: тип в сигнатуре `#[embassy_executor::task]` и у
/// слота узла должен быть конкретным.
{%- if config_store == "eeprom" %}
pub type Settings =
    adapters::settings::Settings<adapters::eeprom::Eeprom<'static, bsp::settings::EepromFlash>>;
{%- else %}
pub type Settings = adapters::settings::Settings<bsp::settings::ConfigFlash>;
{%- endif %}

/// Собирает хранилище над носителем платы.
///
/// # Panics
///
/// Если раздел не годится под `sequential-storage` (меньше двух страниц
/// стирания) — см. `adapters::settings::Settings::new`.
#[must_use]
pub fn new(flash: bsp::settings::ConfigFlash) -> Settings {
{%- if config_store == "eeprom" %}
    // Вся EEPROM, округлённая до страницы обёртки (`Eeprom::PAGE`).
    let eeprom = adapters::eeprom::Eeprom::new(flash);
    let size = eeprom.usable();
    Settings::new(eeprom, 0..size)
{%- else %}
    Settings::new(flash, bsp::settings::RANGE)
{%- endif %}
}
