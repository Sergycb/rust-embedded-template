//! Runtime-конфигурация устройства: адресуемое дерево настроек поверх
//! вложенных структур (`derive(Tree)`), доступ к отдельным "листьям" по
//! JSON-пути, без аллокатора (`json-core`).
//!
//! `cargo run -p domain --example miniconf_settings`

use miniconf::{Tree, json_core};

#[derive(Tree, Default)]
struct Settings {
    sample_rate_hz: u32,
    usart_baud: u32,
}

fn main() {
    let mut settings = Settings::default();

    json_core::set(&mut settings, "/sample_rate_hz", b"1000").expect("valid path/value");
    json_core::set(&mut settings, "/usart_baud", b"115200").expect("valid path/value");

    assert_eq!(settings.sample_rate_hz, 1000);
    assert_eq!(settings.usart_baud, 115_200);

    let mut buf = [0u8; 16];
    let len = json_core::get(&settings, "/sample_rate_hz", &mut buf).expect("valid path");
    assert_eq!(&buf[..len], b"1000");
}
