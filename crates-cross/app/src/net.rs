//! Сетевые службы приложения: то, что `domain` получает входом узла, а знает
//! только этот проект, — по модулю на службу. Стек, на котором службы открывают
//! сокеты, собирает плата (`bsp::net`), и сокеты под них считает она же
//! (`bsp::net::SOCKETS`).
{%- if net_services contains "mqtt" %}

/// Клиент MQTT (`domain::mqtt`, узел `MQTT`): брокер и префикс тем из
/// `mqtt.toml`, размер буферов сокета клиента.
pub mod mqtt {
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
    pub const TCP_BUFFER: usize = 1024;

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
}
{%- endif %}
{%- if net_services contains "rest" %}

/// REST API (`domain::http`, пул узлов `HTTP`): токен доступа и число соединений.
pub mod rest {
    /// Соединений REST API разом — копий пула узлов `HTTP`: переменная среды
    /// `HTTP_CONNECTIONS` из `[env]` `crates-cross/.cargo/config.toml`. Решает
    /// приложение, а не узел: копия узла — одно соединение.
    pub const CONNECTIONS: usize = match usize::from_str_radix(env!("HTTP_CONNECTIONS"), 10) {
        Ok(connections) => connections,
        Err(_) => panic!("HTTP_CONNECTIONS в [env] crates-cross/.cargo/config.toml — не число"),
    };

    /// Токен `Authorization: Bearer` REST API (`domain::http`). Файл в корне
    /// проекта создаёт первая `cargo xtask build` (или `lint cross`), и он
    /// коммитится: токен всё равно лежит открыто в каждом образе.
    pub const TOKEN: &str = include_str!("../../../rest-token.txt").trim_ascii();

    // Пустой файл — не «токена нет», а поломка: `domain::http` такой токен не
    // пустит никого, и лучше узнать это при сборке, чем по `401` на плате.
    const _: () = assert!(
        !TOKEN.is_empty(),
        "rest-token.txt пуст: удалите файл, и `cargo xtask build` создаст новый токен"
    );
}
{%- endif %}
{%- if net_services contains "ssh" %}

/// Консоль SSH (`domain::ssh`, узел `SSH`): вход пользователя{% if ssh_host_key == "file" %} и ключ хоста{% endif %}.
pub mod ssh {
{%- if ssh_auth == "password" %}
    /// Как консоль SSH пускает пользователя: по паролю [`PASSWORD`].
    pub const LOGIN: domain::ssh::Login = domain::ssh::Login::Password(PASSWORD);

    /// Пароль консоли SSH (`domain::ssh::USER`). Файл `ssh-password.txt` в корне
    /// проекта создаёт первая `cargo xtask build`, и он коммитится — как токен
    /// REST: пароль всё равно лежит открыто в каждом образе.
    const PASSWORD: &str = include_str!("../../../ssh-password.txt").trim_ascii();

    // Пустой пароль впустил бы любого с пустым паролем.
    const _: () = assert!(
        !PASSWORD.is_empty(),
        "ssh-password.txt пуст: удалите файл, и `cargo xtask build` создаст новый пароль"
    );
{%- else %}
    /// Как консоль SSH пускает пользователя: по открытому ключу, данному при
    /// генерации (`--define ssh_key=…`). Ключ открытый — его не страшно держать
    /// в коде; сменить — поправить строку.
    pub const LOGIN: domain::ssh::Login = domain::ssh::Login::Key(
        "{{ssh_key}}",
    );
{%- endif %}
{%- if ssh_host_key == "file" %}

    /// Зерно ключа хоста SSH (ed25519): `ssh-host-key.bin` в корне проекта создаёт
    /// первая `cargo xtask build`, и он коммитится. Один ключ на все платы с этой
    /// прошивкой, и лежит он открыто в образе — защита уровня стенда.{% if config == "true" %} Плата
    /// не хранит его сама: раздел настроек забирает узел CANopen.{% endif %}
    pub static HOST_KEY: [u8; 32] = *include_bytes!("../../../ssh-host-key.bin");
{%- endif %}
}
{%- endif %}
