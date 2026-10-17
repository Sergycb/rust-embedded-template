{%- if signed == "true" -%}
use std::path::{Path, PathBuf};

{% endif -%}
fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=memory.x");
    println!(
        "cargo::rustc-link-search={}",
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR задаёт cargo")
    );
{%- if signed == "true" %}

    public_key(&PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR задаёт cargo"),
    ));
{%- endif %}
}
{%- if signed == "true" %}

/// Открытый ключ подписи: из корня проекта в `OUT_DIR`, откуда его забирает
/// `include_bytes!` в src/signature.rs. Через промежуточный файл, а не напрямую,
/// ровно по одной причине: `include_bytes!` выводит длину массива из размера
/// файла, и `OTA_PUBLIC_KEY_IN_IMAGE: [u8; 32]` не собрался бы, не окажись
/// файла на месте (обычное состояние проекта до первой сборки). Здесь длина
/// гарантирована: либо 32 байта ключа, либо 32 нуля.
///
/// Нули означают «ключ ещё не создан», и `domain::ota::boot::screen_update`
/// отвергает с ними любой образ отдельной явной проверкой.
fn public_key(manifest: &Path) {
    /// Имя файла с открытым ключом в корне проекта. Его создаёт и
    /// поддерживает `cargo xtask build`; здесь он только читается.
    const PUBLIC_KEY_FILE: &str = "ota-public-key.bin";

    let root = manifest
        .parent()
        .and_then(Path::parent)
        .expect("boot лежит в crates-cross/boot — два уровня до корня проекта");
    let source = root.join(PUBLIC_KEY_FILE);

    // `rerun-if-changed` только на существующий файл: отсутствующий по такому
    // пути cargo считает вечно устаревшим, и скрипт перезапускался бы на
    // каждой сборке, пересобирая bootloader. Обратная сторона: ключ, положенный
    // МИМО `cargo xtask build`, скрипт не заметит — лечится `cargo clean -p boot`.
    let key = match std::fs::read(&source) {
        Ok(bytes) if bytes.len() == 32 => {
            println!("cargo::rerun-if-changed={}", source.display());
            bytes
        }
        Ok(bytes) => panic!(
            "{} должен быть ровно 32 байта, а в нём {}: удалите файл вместе с ota-signing-key.bin, \
             и `cargo xtask build` создаст пару заново",
            source.display(),
            bytes.len(),
        ),
        // Только «файла нет» означает «ключ ещё не создан». Любая другая ошибка
        // — ключ, который есть, но не прочитался: подставить нули значило бы
        // молча собрать bootloader, отвергающий ЛЮБОЕ обновление.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "cargo::warning=ota-public-key.bin не найден: OTA_PUBLIC_KEY_IN_IMAGE будет нулевым, \
                 bootloader отвергнет любой образ. Ключ создаст первый же `cargo xtask build`."
            );
            vec![0; 32]
        }
        Err(error) => panic!("не прочитать {}: {error}", source.display()),
    };

    // Пишем, только если содержимое изменилось: перезапись файла в OUT_DIR
    // делает устаревшим всё, что от него зависит, — bootloader целиком.
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR задаёт cargo"))
        .join(PUBLIC_KEY_FILE);
    if std::fs::read(&out).ok().as_deref() != Some(key.as_slice()) {
        std::fs::write(&out, key).expect("записать открытый ключ в OUT_DIR");
    }
}
{%- endif %}
