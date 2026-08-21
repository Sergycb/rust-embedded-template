//! Иллюстративный typestate-builder: `build()` доступен только после того,
//! как выставлены все обязательные поля — отсутствующее обязательное поле
//! ловится компилятором, а не паникой/`Result` в рантайме. Не пересекается
//! с `typestate` (тот моделирует переходы поведения, этот — конструирование
//! объекта).
//!
//! `cargo run -p domain --example type_state_builder_config`

use type_state_builder::TypeStateBuilder;

#[derive(TypeStateBuilder)]
struct DeviceConfig {
    #[builder(required)]
    device_id: u32,

    #[builder(required)]
    baud_rate: u32,

    #[builder(default = 3)]
    retry_count: u8,
}

fn main() {
    let config = DeviceConfig::builder()
        .device_id(1)
        .baud_rate(115_200)
        .build();

    assert_eq!(config.device_id, 1);
    assert_eq!(config.baud_rate, 115_200);
    assert_eq!(config.retry_count, 3);
}
