use shadow_rs::ShadowBuilder;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=.git/HEAD");
    println!(
        "cargo::rustc-link-search={}",
        std::env::var("CARGO_MANIFEST_DIR").unwrap()
    );

    ShadowBuilder::builder()
        .build()
        .expect("failed to generate shadow-rs build info");
}
