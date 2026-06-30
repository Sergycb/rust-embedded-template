fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=.git/HEAD");

    // Hardware version: set HW_VERSION env var in build environment, default "1.0".
    let hw_version = "1.0".to_string();
    println!("cargo::rustc-env=HW_VERSION={}", hw_version);

    println!(
        "cargo::rustc-link-search={}",
        std::env::var("CARGO_MANIFEST_DIR").unwrap()
    );

    let profile = env::var("PROFILE").unwrap();

    if profile == "debug" {
        // Включаем фичу "debug" автоматически
        println!("cargo:rustc-cfg=feature=\"debug\"");
    } else if profile == "release" {
        // Включаем фичу "release" автоматически
        println!("cargo:rustc-cfg=feature=\"release\"");
    }
}
