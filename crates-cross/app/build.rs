use std::path::{Path, PathBuf};

use shadow_rs::ShadowBuilder;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    // Без этой строки правка раскладки памяти не вызывает перелинковку, и
    // `build`/`flash` продолжают работать со старой — молча. Актуально ровно
    // тогда, когда `memory.x` правят руками: у `boot` и `target-tests` такая
    // строка есть с самого начала.
    println!("cargo::rerun-if-changed=memory.x");

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
{%- if signed == "true" %}

    public_key(&manifest);
{%- endif %}
}
{%- if signed == "true" %}

/// Открытый ключ подписи: из корня проекта в OUT_DIR, откуда его забирает
/// `include_bytes!` в src/ota.rs. Через промежуточный файл, а не напрямую,
/// ровно по одной причине: `include_bytes!` выводит длину массива из размера
/// файла, и `PUBLIC_KEY: [u8; 32]` не собрался бы, не окажись файла на месте
/// (обычное состояние проекта до первой сборки). Здесь длина гарантирована:
/// либо 32 байта ключа, либо 32 нуля.
///
/// Нули означают «ключ ещё не создан», и `domain::update::apply_signed`
/// отказывается работать с ними отдельной явной проверкой.
fn public_key(manifest: &Path) {
    /// Имя файла с открытым ключом в корне проекта. Его создаёт и
    /// поддерживает `cargo xtask build`; здесь он только читается.
    const PUBLIC_KEY_FILE: &str = "ota-public-key.bin";

    let root = manifest
        .parent()
        .and_then(Path::parent)
        .expect("app лежит в crates-cross/app — два уровня до корня проекта");
    let source = root.join(PUBLIC_KEY_FILE);

    // `rerun-if-changed` только на существующий файл: отсутствующий по такому
    // пути cargo считает вечно устаревшим, и скрипт перезапускался бы на
    // каждой сборке, пересобирая прошивку. Обратная сторона: ключ, положенный
    // МИМО `cargo xtask build`, скрипт не заметит — лечится `cargo clean -p app`.
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
        // молча собрать прошивку, отвергающую ЛЮБОЕ обновление.
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
    // делает устаревшим всё, что от него зависит, — прошивку целиком.
    let out = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR задаёт cargo"))
        .join(PUBLIC_KEY_FILE);
    if std::fs::read(&out).ok().as_deref() != Some(key.as_slice()) {
        std::fs::write(&out, key).expect("записать открытый ключ в OUT_DIR");
    }
}
{%- endif %}
