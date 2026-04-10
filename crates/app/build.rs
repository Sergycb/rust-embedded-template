fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=.git/HEAD");

    // Firmware version from git describe (tag + commit + dirty flag).
    let git_version = std::process::Command::new("git")
        .args(["describe", "--tags", "--always", "--dirty"])
        .current_dir(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR not set"))
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|| "unknown".to_string());
    println!("cargo::rustc-env=GIT_VERSION={}", git_version);

	println!("cargo::rustc-link-search={}", std::env::var("CARGO_MANIFEST_DIR").unwrap());
}
