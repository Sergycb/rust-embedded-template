fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=.git/HEAD");

    println!(
        "cargo::rustc-link-search={}",
        std::env::var("CARGO_MANIFEST_DIR").unwrap()
    );

    let profile = std::env::var("PROFILE").unwrap();

    if profile == "debug" {
        // Включаем фичу "debug" автоматически
        println!("cargo:rustc-cfg=feature=\"debug\"");
    } else if profile == "release" {
        // Включаем фичу "release" автоматически
        println!("cargo:rustc-cfg=feature=\"release\"");
    }
}
