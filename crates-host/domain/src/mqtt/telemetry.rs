//! Телеметрия: как часто и что плата публикует в `telemetry`.

use embassy_time::Duration;

/// Как часто плата публикует `telemetry`.
pub const TELEMETRY_INTERVAL: Duration = Duration::from_secs(10);

/// Тело `telemetry`: JSON-объект со временем работы.
///
/// # Errors
///
/// Ошибка записи в `out`: объект не влез в буфер.
pub fn write_telemetry(out: &mut impl core::fmt::Write, uptime_s: u64) -> core::fmt::Result {
    // Скобки — `write_char`: это исходник шаблона, см. `device::write_info`.
    out.write_char('{')?;
    write!(out, r#""uptime_s":{uptime_s}"#)?;
    out.write_char('}')
}
