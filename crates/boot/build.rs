fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=memory.x");

    // Bootloader version: set BOOT_VERSION env var in build environment, default "1.0".
    let boot_version = "1.0".to_string();
    println!("cargo::rustc-env=BOOT_VERSION={}", boot_version);

	println!("cargo::rustc-link-search={}", std::env::var("CARGO_MANIFEST_DIR").unwrap());
}
