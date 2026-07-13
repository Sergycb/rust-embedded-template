use std::{env, path::PathBuf};

use xshell::cmd;

const CHIP: &str = "{{chip}}";

fn main() -> Result<(), anyhow::Error> {
    let args = env::args().skip(1).collect::<Vec<_>>();
    let args = args.iter().map(|s| &**s).collect::<Vec<_>>();
    let sh = xshell::Shell::new()?;

    match &args[..] {
        ["test", "all"] => test_all(&sh),
        ["test", "host"] => test_host(&sh),
        ["test", "host-target"] => test_host_target(&sh),
        ["test", "target"] => test_target(&sh),
        ["build"] => build(&sh),
        ["run", "debug"] => run_debug(&sh),
        ["run", "release"] => run_release(&sh),
        ["flash", "debug"] => flash_debug(&sh),
        ["flash", "release"] => flash_release(&sh),
        ["lint"] => lint_host(&sh),
        ["lint", "cross"] => lint_cross(&sh),
        ["deny"] => deny(&sh),
        _ => {
            println!("USAGE cargo xtask test [all|host|host-target|target]");
            println!("      cargo xtask build");
            println!("      cargo xtask run [debug|release]");
            println!("      cargo xtask flash [debug|release]");
            println!("      cargo xtask lint [cross]");
            println!("      cargo xtask deny");
            Ok(())
        }
    }
}

fn run_debug(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    flash_boot(sh, "debug")?;
    let _p = sh.push_dir(root_dir().join("cross/app"));
    cmd!(sh, "cargo run").run()?;
    Ok(())
}

fn run_release(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    flash_boot(sh, "release")?;
    let _p = sh.push_dir(root_dir().join("cross/app"));
    cmd!(sh, "cargo run --release").run()?;
    Ok(())
}

fn flash_debug(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    flash_boot(sh, "debug")?;
    flash_app(sh, "debug")?;
    Ok(())
}

fn flash_release(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    flash_boot(sh, "release")?;
    flash_app(sh, "release")?;
    Ok(())
}

fn build(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir().join("cross"));
    cmd!(sh, "cargo build").run()?;
    cmd!(sh, "cargo build --release").run()?;
    Ok(())
}

fn test_all(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    test_host(sh)?;
    test_target(sh)?;
    test_host_target(sh)
}

fn test_host(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir());
    cmd!(
        sh,
        "cargo nextest run --workspace --exclude host-target-tests --release --features domain/log,domain/std"
    )
    .run()?;
    Ok(())
}

fn test_host_target(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    flash_boot(sh, "release")?;
    flash_app(sh, "release")?;
    {
        let _p = sh.push_dir(root_dir().join("host-target-tests"));
        cmd!(sh, "cargo nextest run --features release").run()?;
    }
    Ok(())
}

fn test_target(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir().join("cross/target-tests"));
    cmd!(sh, "cargo test").run()?;
    Ok(())
}

fn lint_host(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir());
    cmd!(sh, "cargo fmt --check").run()?;
    cmd!(
        sh,
        "cargo clippy --workspace --all-targets --features domain/std,domain/log -- -D warnings"
    )
    .run()?;
    Ok(())
}

fn lint_cross(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir().join("cross"));
    cmd!(sh, "cargo fmt --check").run()?;
    // target-tests has no default targets of its own until it grows an embedded-test
    // harness, so --all-targets is intentionally omitted here (it would otherwise try
    // and fail to build its empty `tests/test.rs` as a no_std test binary).
    cmd!(sh, "cargo clippy --workspace -- -D warnings").run()?;
    // Second pass, release only, scoped to app+boot: the panic handler and defmt
    // transport there are chosen via `#[cfg(debug_assertions)]`, not a Cargo feature,
    // so the release branch is only type-checked under this profile. bsp/target-tests
    // have no such branches, so re-linting them here would just repeat the first pass.
    cmd!(sh, "cargo clippy -p app -p boot --release -- -D warnings").run()?;
    Ok(())
}

fn deny(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir());
    cmd!(sh, "cargo deny check").run()?;
    Ok(())
}

fn flash_app(sh: &xshell::Shell, profile: &str) -> Result<(), anyhow::Error> {
    flash(sh, "app", profile)
}

fn flash_boot(sh: &xshell::Shell, profile: &str) -> Result<(), anyhow::Error> {
    flash(sh, "boot", profile)
}

fn flash(sh: &xshell::Shell, package: &str, profile: &str) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir().join("cross"));
    match profile {
        "release" => cmd!(sh, "cargo flash -p {package} --release --chip {CHIP}").run()?,
        "debug" => cmd!(sh, "cargo flash -p {package} --chip {CHIP}").run()?,
        other => anyhow::bail!("unknown profile: {other}"),
    }
    Ok(())
}

fn root_dir() -> PathBuf {
    let mut xtask_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    xtask_dir.pop();
    xtask_dir
}
