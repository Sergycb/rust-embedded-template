//! Телеметрия: как часто и что плата публикует в `telemetry`.

use embassy_time::Duration;

/// Как часто плата публикует `telemetry`.
pub const TELEMETRY_INTERVAL: Duration = Duration::from_secs(10);

/// Тело `telemetry`: время работы текстом.
///
/// # Errors
///
/// Ошибка записи в `out`: текст не влез в буфер.
pub fn write_telemetry(out: &mut impl core::fmt::Write, uptime_s: u64) -> core::fmt::Result {
    writeln!(out, "uptime_s {uptime_s}")
}
