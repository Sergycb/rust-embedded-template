//! Клиент MQTT (`domain::mqtt`, узел `MQTT`): брокер и префикс тем из
//! `mqtt.toml`, буферы сокета клиента.

use static_cell::ConstStaticCell;

/// Брокер MQTT (`domain::mqtt`) из `mqtt.toml` в корне проекта: файл разбирает
/// и проверяет `build.rs`, сюда он приходит переменными окружения компиляции.
pub const BROKER: domain::mqtt::Endpoint = domain::mqtt::Endpoint {
    addr: [
        octet(env!("MQTT_HOST_0")),
        octet(env!("MQTT_HOST_1")),
        octet(env!("MQTT_HOST_2")),
        octet(env!("MQTT_HOST_3")),
    ],
    port: port(env!("MQTT_PORT")),
    user: env!("MQTT_USER"),
    password: env!("MQTT_PASSWORD"),
};

/// Префикс тем MQTT (`<prefix>/<uid>/…`) из `mqtt.toml`.
pub const PREFIX: &str = env!("MQTT_PREFIX");

// Предел префикса `build.rs` держит своим числом: `domain` он не видит.
const _: () = assert!(
    PREFIX.len() <= domain::mqtt::PREFIX_MAX,
    "prefix в mqtt.toml длиннее domain::mqtt::PREFIX_MAX — поправьте предел в build/mqtt.rs"
);
// Так же — предел входа: `CONNECT` с ним держит место в `domain::mqtt::TX_BUFFER`.
const _: () = assert!(
    BROKER.user.len() + BROKER.password.len() <= domain::mqtt::AUTH_MAX,
    "user и password в mqtt.toml длиннее domain::mqtt::AUTH_MAX — поправьте предел в build/mqtt.rs"
);

/// Буферы приёма и передачи сокета MQTT, байт: пакеты `minimq` ограничены
/// его буферами (`domain::mqtt::RX_BUFFER`, `TX_BUFFER`), сокету хватает окна.
const TCP_BUFFER: usize = 1024;

static TCP_RX: ConstStaticCell<[u8; TCP_BUFFER]> = ConstStaticCell::new([0; TCP_BUFFER]);
static TCP_TX: ConstStaticCell<[u8; TCP_BUFFER]> = ConstStaticCell::new([0; TCP_BUFFER]);

/// Память соединения узла MQTT с брокером — буферы сокета TCP-клиента;
/// собирает клиент сам узел.
///
/// # Panics
///
/// При втором вызове: буферы у клиента одни, а узел MQTT — один.
pub fn buffers() -> adapters::net::TcpClientBuffers {
    adapters::net::TcpClientBuffers {
        rx: TCP_RX.take(),
        tx: TCP_TX.take(),
    }
}

/// Октет адреса брокера из строки `build.rs`.
const fn octet(text: &str) -> u8 {
    let Ok(octet) = u8::from_str_radix(text, 10) else {
        panic!("MQTT_HOST_* — октеты от build.rs");
    };
    octet
}

/// Порт брокера из строки `build.rs`.
const fn port(text: &str) -> u16 {
    let Ok(port) = u16::from_str_radix(text, 10) else {
        panic!("MQTT_PORT — число от build.rs");
    };
    port
}
