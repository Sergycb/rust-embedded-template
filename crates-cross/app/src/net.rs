//! Сетевые службы приложения: то, что `domain` получает входом узла, а знает
//! только этот проект, — {% if net_services contains "rest" %}токен REST API{% endif %}{% if net_services contains "rest" and net_services contains "ssh" %}, {% endif %}{% if net_services contains "ssh" %}вход и ключ консоли SSH{% endif %}{% if net_services contains "mqtt" %}{% if net_services contains "rest" or net_services contains "ssh" %}, {% endif %}брокер MQTT и сокет его клиента{% endif %}.
//! Стек, на котором службы открывают сокеты, собирает плата (`bsp::net`), и
//! сокеты под них он заводит по числу из генерации (`NET_SOCKETS`).
{%- if net_services contains "mqtt" %}

use static_cell::ConstStaticCell;
{%- endif %}
{%- if net_services contains "rest" %}

/// Токен `Authorization: Bearer` REST API (`domain::http`). Файл в корне
/// проекта создаёт первая `cargo xtask build` (или `lint cross`), и он
/// коммитится: токен всё равно лежит открыто в каждом образе.
pub const REST_TOKEN: &str = include_str!("../../../rest-token.txt").trim_ascii();

// Пустой файл — не «токена нет», а поломка: `domain::http` такой токен не
// пустит никого, и лучше узнать это при сборке, чем по `401` на плате.
const _: () = assert!(
    !REST_TOKEN.is_empty(),
    "rest-token.txt пуст: удалите файл, и `cargo xtask build` создаст новый токен"
);
{%- endif %}
{%- if net_services contains "ssh" %}
{%- if ssh_auth == "password" %}

/// Пароль консоли SSH (`domain::ssh::USER`). Файл `ssh-password.txt` в корне
/// проекта создаёт первая `cargo xtask build`, и он коммитится — как токен
/// REST: пароль всё равно лежит открыто в каждом образе.
const SSH_PASSWORD: &str = include_str!("../../../ssh-password.txt").trim_ascii();

// Пустой пароль впустил бы любого с пустым паролем.
const _: () = assert!(
    !SSH_PASSWORD.is_empty(),
    "ssh-password.txt пуст: удалите файл, и `cargo xtask build` создаст новый пароль"
);

/// Как консоль SSH пускает пользователя.
pub const SSH_LOGIN: domain::ssh::Login = domain::ssh::Login::Password(SSH_PASSWORD);
{%- else %}

/// Как консоль SSH пускает пользователя: по открытому ключу, данному при
/// генерации (`--define ssh_key=…`). Ключ открытый — его не страшно держать
/// в коде; сменить — поправить строку.
pub const SSH_LOGIN: domain::ssh::Login = domain::ssh::Login::Key(
    "{{ssh_key}}",
);
{%- endif %}
{%- if ssh_host_key == "file" %}

/// Зерно ключа хоста SSH (ed25519): `ssh-host-key.bin` в корне проекта создаёт
/// первая `cargo xtask build`, и он коммитится. Один ключ на все платы с этой
/// прошивкой, и лежит он открыто в образе — защита уровня стенда.{% if config == "true" %} Плата
/// не хранит его сама: раздел настроек забирает узел CANopen.{% endif %}
pub static SSH_HOST_KEY: [u8; 32] = *include_bytes!("../../../ssh-host-key.bin");
{%- endif %}
{%- endif %}
{%- if net_services contains "mqtt" %}

/// Брокер MQTT (`domain::mqtt`) из `mqtt.toml` в корне проекта: файл разбирает
/// и проверяет `build.rs`, сюда он приходит переменными окружения компиляции.
pub const MQTT_BROKER: domain::mqtt::Endpoint = domain::mqtt::Endpoint {
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
pub const MQTT_PREFIX: &str = env!("MQTT_PREFIX");

// Предел префикса `build.rs` держит своим числом: `domain` он не видит.
const _: () = assert!(
    MQTT_PREFIX.len() <= domain::mqtt::PREFIX_MAX,
    "prefix в mqtt.toml длиннее domain::mqtt::PREFIX_MAX — поправьте предел в build/mqtt.rs"
);
// Так же — предел входа: `CONNECT` с ним держит место в `domain::mqtt::TX_BUFFER`.
const _: () = assert!(
    MQTT_BROKER.user.len() + MQTT_BROKER.password.len() <= domain::mqtt::AUTH_MAX,
    "user и password в mqtt.toml длиннее domain::mqtt::AUTH_MAX — поправьте предел в build/mqtt.rs"
);

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

/// Буферы приёма и передачи сокета MQTT, байт: пакеты `minimq` ограничены
/// его буферами (`domain::mqtt::RX_BUFFER`, `TX_BUFFER`), сокету хватает окна.
pub const MQTT_TCP_BUFFER: usize = 1024;

static MQTT_TCP_RX: ConstStaticCell<[u8; MQTT_TCP_BUFFER]> =
    ConstStaticCell::new([0; MQTT_TCP_BUFFER]);
static MQTT_TCP_TX: ConstStaticCell<[u8; MQTT_TCP_BUFFER]> =
    ConstStaticCell::new([0; MQTT_TCP_BUFFER]);

/// Соединение узла MQTT с брокером: TCP-клиент на стеке со своими буферами.
///
/// # Panics
///
/// При втором вызове: буферы у клиента одни, а узел MQTT — один.
pub fn mqtt_link(stack: bsp::net::Stack) -> adapters::net::TcpClient {
    adapters::net::TcpClient::new(stack, MQTT_TCP_RX.take(), MQTT_TCP_TX.take())
}
{%- endif %}
