//! Сетевой стек приложения: `embassy-net` над Ethernet платы
//! (`bsp::Drivers::eth`).
//!
//! Число сокетов, адресация и память стека — решение приложения, а не
//! платы: `bsp` отдаёт только устройство. Стек и фоновая работа над ним
//! рождаются одним `embassy_net::new` и расходятся по разным узлам: фон — в
//! узел `NET`, стек — сокетам протоколов (`crate::link`) или вашему коду.

{%- assign xs = links | split: "," %}
{%- assign udp_protocol = "false" %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "udp" and f[3] != "none" %}{% assign udp_protocol = "true" %}{% endif %}
{%- endfor %}
{%- if udp_protocol == "true" %}
/// Описание датаграммы в буфере UDP-сокета — для буферов, которые приложение
/// отдаёт сокету протокола (`crate::link`).
pub use embassy_net::udp::PacketMetadata;
{%- endif %}
use embassy_net::{Config, DhcpConfig, StackResources};
use static_cell::ConstStaticCell;

/// Сокетов в стеке: DHCP-клиент и по сокету на сетевой транспорт — у
/// транспорта с протоколом его открывает `crate::link`, без протокола — ваш
/// код на `Parts::net`{% if net_services contains "rest" %}, — и по сокету на соединение REST API
/// (`domain::http::CONNECTIONS`){% endif %}{% if net_services contains "ssh" %}, — и один на консоль SSH{% endif %}. Слот — около 500 байт оперативной памяти, поэтому без
/// запаса: добавите ещё сокет (или фичу `dns` у `embassy-net` — она тоже
/// занимает слот) — прибавьте один здесь.
const SOCKETS: usize = {{ net_sockets | plus: 1 }}{% if net_services contains "rest" %} + domain::http::CONNECTIONS{% endif %}{% if net_services contains "ssh" %} + 1{% endif %};
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

// Память стека: `embassy_net::new` держит её `&'static mut`. `ConstStaticCell`
// — значения известны при компиляции и лежат в `.bss`, а не собираются на
// стеке и копируются при старте.
static RESOURCES: ConstStaticCell<StackResources<SOCKETS>> =
    ConstStaticCell::new(StackResources::new());

{%- if protocol_sockets != "0" %}

/// Сетевой стек, на котором открываются сокеты протоколов (`crate::link`).
pub type Stack = embassy_net::Stack<'static>;
{%- endif %}

/// Фоновая работа стека (узел `NET`) и сам стек.
pub type Net = adapters::net::Net<bsp::net::Device>;

/// Собирает стек над устройством платы. Адрес — по DHCP; затравка случайных
/// чисел стека (порты, номера последовательностей TCP) — `seed`.
pub fn new(device: bsp::net::Device, seed: u64) -> Net {
    let config = Config::dhcpv4(DhcpConfig::default());
    let (stack, runner) = embassy_net::new(device, config, RESOURCES.take(), seed);
    Net { runner, stack }
}
{%- if rng_irq == "" %}

/// Затравка стека из заводского номера — для чипов без RNG. Не случайна: у
/// одной платы она одна и та же после каждого сброса.
pub fn seed_from_uid(uid: [u8; 12]) -> u64 {
    let mut bytes = [0; 8];
    bytes.copy_from_slice(&uid[..8]);
    u64::from_le_bytes(bytes) ^ u64::from(u32::from_le_bytes([uid[8], uid[9], uid[10], uid[11]]))
}
{%- endif %}
