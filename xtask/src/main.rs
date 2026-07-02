use std::{env, path::PathBuf};

use xshell::cmd;

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
        _ => {
            println!("USAGE cargo xtask test [all|host|host-target|target]");
            println!("      cargo xtask build");
            println!("      cargo xtask run [debug|release]");
            println!("      cargo xtask flash [debug|release]");
            Ok(())
        }
    }
}

fn run_debug(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    flash_boot(sh, "debug")?;
    let _p = sh.push_dir(root_dir().join("cross/app"));
    cmd!(sh, "cargo run --features debug").run()?;
    Ok(())
}

fn run_release(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    flash_boot(sh, "release")?;
    let _p = sh.push_dir(root_dir().join("cross/app"));
    cmd!(sh, "cargo run --release --features release").run()?;
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
    cmd!(sh, "cargo build --features debug").run()?;
    cmd!(sh, "cargo build --release --features release").run()?;
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

fn flash_app(sh: &xshell::Shell, features: &str) -> Result<(), anyhow::Error> {
    flash(sh, "app", features)
}

fn flash_boot(sh: &xshell::Shell, features: &str) -> Result<(), anyhow::Error> {
    flash(sh, "boot", features)
}

fn flash(sh: &xshell::Shell, package: &str, features: &str) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir().join("cross"));
    match features {
        "release" => cmd!(
            sh,
            "cargo flash -p {package} --release --features release --chip STM32H723ZETx"
        )
        .run()?,
        "debug" => cmd!(
            sh,
            "cargo flash -p {package} --features debug --chip STM32H723ZETx"
        )
        .run()?,
        other => anyhow::bail!("unknown features profile: {other}"),
    }
    Ok(())
}

fn root_dir() -> PathBuf {
    let mut xtask_dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    xtask_dir.pop();
    xtask_dir
}
