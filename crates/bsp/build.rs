fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=.git/HEAD");

    // Hardware version: set HW_VERSION env var in build environment, default "1.0".
    let hw_version = "1.0".to_string();
    println!("cargo::rustc-env=HW_VERSION={}", hw_version);
}
