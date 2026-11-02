//! Тестовый образ `cargo xtask test host-target`: та же прошивка, что `app`, но
//! собранная с фичей `unconfirmed` (`required-features` в Cargo.toml) — себя
//! не подтверждает, и следующий сброс её откатывает (docs/ota.md, «Тесты OTA
//! по протоколу»). Различие — в библиотеке пакета (`cfg!(feature =
//! "unconfirmed")`), здесь только точка входа.
#![no_std]
#![no_main]

#[cortex_m_rt::entry]
fn main() -> ! {
    app::run()
}
