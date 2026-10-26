//! Консоль SSH (`domain::ssh`, узел `SSH`): вход пользователя{% if ssh_host_key == "file" %} и ключ хоста{% endif %}.
{%- if ssh_auth == "password" %}

/// Как консоль SSH пускает пользователя: по паролю [`PASSWORD`].
pub const LOGIN: domain::ssh::Login = domain::ssh::Login::Password(PASSWORD);

/// Пароль консоли SSH (`domain::ssh::USER`). Файл `ssh-password.txt` в корне
/// проекта создаёт первая `cargo xtask build`, и он коммитится — как токен
/// REST: пароль всё равно лежит открыто в каждом образе.
const PASSWORD: &str = include_str!("../../../../ssh-password.txt").trim_ascii();

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
pub static HOST_KEY: [u8; 32] = *include_bytes!("../../../../ssh-host-key.bin");
{%- endif %}
