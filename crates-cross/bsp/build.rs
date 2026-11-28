use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");

    let manifest = PathBuf::from(
        std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR задаёт cargo"),
    );

    // `[env]` из `crates-cross/.cargo/config.toml` читает `env!` в `bsp`
    // (`HTTP_CONNECTIONS` в src/ethernet.rs); cargo не считает правку
    // `config.toml` изменением входа build-скрипта, поэтому переменную
    // объявляем явно.
    println!("cargo::rerun-if-env-changed=HTTP_CONNECTIONS");
    if let Some(config) = manifest.parent().map(|dir| dir.join(".cargo/config.toml"))
        && config.exists()
    {
        println!("cargo::rerun-if-changed={}", config.display());
    }
}
