{%- if net_services contains "mqtt" -%}
#[path = "build/mqtt.rs"]
mod mqtt;

{% endif -%}
use std::path::{Path, PathBuf};

use shadow_rs::ShadowBuilder;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    // Без этой строки правка раскладки памяти не вызывает перелинковку, и
    // `build`/`flash` продолжают работать со старой — молча. Актуально ровно
    // тогда, когда `memory.x` правят руками: у `boot` и `target-tests` такая
    // строка есть с самого начала.
    println!("cargo::rerun-if-changed=memory.x");
{%- if net_services contains "rest" %}
    require("rest-token.txt", "токена REST API");
{%- endif %}
{%- if ssh_auth == "password" %}
    require("ssh-password.txt", "пароля консоли SSH");
{%- endif %}
{%- if ssh_host_key == "file" %}
    require("ssh-host-key.bin", "ключа хоста SSH");
{%- endif %}
{%- if net_services contains "mqtt" %}
    mqtt_config();
{%- endif %}

    let manifest = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR задаёт cargo"),
    );
    println!("cargo::rustc-link-search={}", manifest.display());

    // Смена коммита должна пересобирать build-info (`shadow-rs` подставляет
    // SHORT_COMMIT в баннер старта). Путь считается от корня проекта, а не
    // относительный `.git/HEAD`, как было раньше: build-скрипт исполняется в
    // каталоге крейта, где `.git` нет, а НЕСУЩЕСТВУЮЩИЙ путь в
    // `rerun-if-changed` cargo считает вечно устаревшим — скрипт перезапускался
    // на каждой сборке и тянул за собой полную пересборку прошивки.
    //
    // Проверка существования нужна по той же причине: проект могли распаковать
    // без git.
    let head = manifest
        .parent()
        .and_then(Path::parent)
        .map(|root| root.join(".git").join("HEAD"));
    if let Some(head) = head.filter(|path| path.exists()) {
        println!("cargo::rerun-if-changed={}", head.display());
    }

    ShadowBuilder::builder()
        .build()
        .expect("failed to generate shadow-rs build info");
}
{%- if net_services contains "rest" or ssh_auth == "password" or ssh_host_key == "file" or net_services contains "mqtt" %}

/// Секрет сетевой службы из корня проекта, который `src/net.rs` вшивает
/// `include_str!`/`include_bytes!`. Без файла компилятор сказал бы только
/// «couldn't read»; файл создаёт `cargo xtask` (`build`, `lint cross`,
/// прошивка), а голый `cargo build`/`cargo run` в свежем проекте — нет.
fn require(file: &str, what: &str) {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(file);
    println!("cargo::rerun-if-changed={}", path.display());
    assert!(
        path.exists(),
        "нет {} — {what}. Его создаёт `cargo xtask build` (или `cargo xtask lint cross`); \
         запустите её один раз из корня проекта и закоммитьте файл",
        path.display()
    );
}
{%- endif %}
{%- if net_services contains "mqtt" %}

/// Брокер MQTT из `mqtt.toml` в корне проекта — переменными окружения
/// компиляции, из которых `src/net/mqtt.rs` собирает `BROKER` и `PREFIX`.
/// Битый файл роняет сборку с именем поля, а не плату на старте.
fn mqtt_config() {
    require("mqtt.toml", "настроек брокера MQTT");
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../mqtt.toml");
    let config = std::fs::read_to_string(&path)
        .map_err(|err| err.to_string())
        .and_then(|text| mqtt::parse_mqtt_toml(&text))
        .unwrap_or_else(|err| panic!("{}: {err}", path.display()));
    for (i, octet) in config.host.octets().iter().enumerate() {
        println!("cargo::rustc-env=MQTT_HOST_{i}={octet}");
    }
    println!("cargo::rustc-env=MQTT_PORT={}", config.port);
    println!("cargo::rustc-env=MQTT_USER={}", config.user);
    println!("cargo::rustc-env=MQTT_PASSWORD={}", config.password);
    println!("cargo::rustc-env=MQTT_PREFIX={}", config.prefix);
{%- if ota == "true" %}
    // `cmd/reboot` перезагружает плату: без входа на брокере его может
    // прислать кто угодно, кто до брокера дотянулся.
    if config.user.is_empty() {
        println!(
            "cargo::warning=mqtt.toml: user пуст — cmd/reboot примет любой клиент брокера; \
             ограничьте доступ ACL (docs/modules/app-mqtt.md)"
        );
    }
{%- endif %}
}
{%- endif %}
