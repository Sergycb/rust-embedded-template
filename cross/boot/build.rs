fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=memory.x");
    println!(
        "cargo::rustc-link-search={}",
        std::env::var("CARGO_MANIFEST_DIR").unwrap()
    );
}
