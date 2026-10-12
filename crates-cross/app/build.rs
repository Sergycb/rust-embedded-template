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
}
