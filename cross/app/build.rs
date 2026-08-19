use shadow_rs::ShadowBuilder;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=.git/HEAD");
    // Без этой строки правка раскладки памяти не вызывает перелинковку, и
    // `build`/`size`/`flash` продолжают работать со старой — молча. Актуально
    // ровно тогда, когда `memory.x` правят руками: у `boot` и `target-tests`
    // такая строка есть с самого начала.
    println!("cargo::rerun-if-changed=memory.x");
    println!(
        "cargo::rustc-link-search={}",
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR задаёт cargo")
    );

    ShadowBuilder::builder()
        .build()
        .expect("failed to generate shadow-rs build info");
}
