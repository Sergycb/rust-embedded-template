fn main() {
    println!("cargo::rustc-link-arg=-Tembedded-test.x");
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=memory.x");
    let manifest_dir = std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR не задан");
    println!("cargo::rustc-link-search={manifest_dir}");
}
