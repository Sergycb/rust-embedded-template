use std::{env, fs, path::PathBuf};

/// Имя файла с открытым ключом в корне проекта. Его создаёт и поддерживает
/// `cargo xtask build`; здесь он только читается.
const PUBLIC_KEY_FILE: &str = "ota-public-key.bin";

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=.git/HEAD");

    let manifest =
        PathBuf::from(env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR задаёт cargo"));
    println!("cargo::rustc-link-search={}", manifest.display());

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
    println!("cargo::rerun-if-changed={}", source.display());

    let key = match fs::read(&source) {
        Ok(bytes) if bytes.len() == 32 => bytes,
        Ok(bytes) => panic!(
            "{} должен быть ровно 32 байта, а в нём {}: удалите файл вместе с ota-signing-key.bin, \
             и `cargo xtask build` создаст пару заново",
            source.display(),
            bytes.len(),
        ),
        Err(_) => vec![0; 32],
    };

    let out = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR задаёт cargo"));
    fs::write(out.join(PUBLIC_KEY_FILE), key).expect("записать открытый ключ в OUT_DIR");
}
