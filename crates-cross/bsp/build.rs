use std::path::PathBuf;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");

    let manifest =
        PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR задаёт cargo"));

    // `[env]` из `crates-cross/.cargo/config.toml` читают `env!` в `bsp`;
    // cargo не считает правку `config.toml` изменением входа build-скрипта,
    // поэтому переменные объявляем явно.
    for var in [
        "WATCHDOG_TIMEOUT_US",
        "STATE_ERASE_SIZE",
        "SWAP_PAGE_SIZE",
        "EXT_PAGE_SIZE",
        "HTTP_CONNECTIONS",
    ] {
        println!("cargo::rerun-if-env-changed={var}");
    }
    if let Some(config) = manifest.parent().map(|dir| dir.join(".cargo/config.toml")) {
        if config.exists() {
            println!("cargo::rerun-if-changed={}", config.display());
        }
    }
}
