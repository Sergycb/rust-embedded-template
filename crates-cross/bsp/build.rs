use std::{env, path::PathBuf};
{%- if signed == "true" %}
// Только ради работы с файлом открытого ключа ниже — в проектах без подписи
// этого блока нет вовсе, и неиспользованный импорт ронял бы сборку.
use std::fs;

/// Имя файла с открытым ключом в корне проекта. Его создаёт и поддерживает
/// `cargo xtask build`; здесь он только читается.
const PUBLIC_KEY_FILE: &str = "ota-public-key.bin";
{%- endif %}

fn main() {
    println!("cargo::rerun-if-changed=build.rs");

    let manifest =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR задаёт cargo"));
    println!("cargo::rustc-link-search={}", manifest.display());
{%- if signed == "true" %}

    // Открытый ключ подписи: из корня проекта в OUT_DIR, откуда его забирает
    // `include_bytes!` в src/ota.rs. Через промежуточный файл, а не напрямую,
    // ровно по одной причине: `include_bytes!` выводит длину массива из
    // размера файла, и `PUBLIC_KEY: [u8; 32]` не собрался бы, не окажись файла
    // на месте (обычное состояние проекта до первой сборки). Здесь длина
    // гарантирована: либо 32 байта ключа, либо 32 нуля.
    //
    // Нули означают «ключ ещё не создан», и `verify_and_mark_updated`
    // отказывается работать с ними отдельной явной проверкой — см. src/ota.rs.
    let root = manifest
        .parent()
        .and_then(|crates_cross| crates_cross.parent())
        .expect("bsp лежит в crates-cross/bsp — два уровня до корня проекта");
    let source = root.join(PUBLIC_KEY_FILE);

    // `rerun-if-changed` только на существующий файл, и это не микрооптимизация.
    // Cargo считает отсутствующий по такому пути файл вечно устаревшим: скрипт
    // перезапускался бы на каждой сборке, а вместе с ним пересобирались бы bsp,
    // app, boot и target-tests. Подпись по умолчанию выключена, то есть ключа
    // нет никогда — инкрементальная сборка перестала бы существовать у всех.
    //
    // Обратная сторона: если ключ появился МИМО `cargo xtask build` (положили
    // руками, вытащили из другого клона), скрипт об этом не узнает. Лечится
    // `cargo clean -p bsp` — или тем, что ключ обычно и создаёт сам `build`,
    // который делает это до компиляции.
    let key = match fs::read(&source) {
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
        // — права, занятый файл, сбой ввода-вывода — это ключ, который есть, но
        // не прочитался: подставить вместо него нули значило бы молча собрать
        // прошивку, отвергающую ЛЮБОЕ обновление, и узнать об этом на уже
        // прошитом устройстве.
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            println!(
                "cargo::warning=ota-public-key.bin не найден: PUBLIC_KEY будет нулевым, проверка \
                 подписи откажет. Ключ создаст первый же `cargo xtask build`."
            );
            vec![0; 32]
        }
        Err(error) => panic!("не прочитать {}: {error}", source.display()),
    };

    // Пишем, только если содержимое изменилось: перезапись файла в OUT_DIR
    // делает устаревшим всё, что от него зависит, и один лишний перезапуск
    // скрипта превращался бы в полную пересборку прошивки.
    let out =
        PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR задаёт cargo")).join(PUBLIC_KEY_FILE);
    if fs::read(&out).ok().as_deref() != Some(key.as_slice()) {
        fs::write(&out, key).expect("записать открытый ключ в OUT_DIR");
    }
{%- endif %}
}
