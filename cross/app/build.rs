use shadow_rs::ShadowBuilder;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=.git/HEAD");
    println!(
        "cargo::rustc-link-search={}",
        std::env::var("CARGO_MANIFEST_DIR").unwrap()
    );

    // `defmt.x` генерируется build-скриптом самого `defmt` только когда он
    // реально слинкован — подключать его безусловно (например, через
    // .cargo/config.toml) ломает release-профиль (только `log`, без defmt).
    if std::env::var("CARGO_FEATURE_DEFMT").is_ok() {
        println!("cargo::rustc-link-arg=-Tdefmt.x");
    }

    ShadowBuilder::builder()
        .build()
        .expect("failed to generate shadow-rs build info");
}
