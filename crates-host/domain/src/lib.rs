#![cfg_attr(not(test), no_std)]

// cfg(kani) объявлен в [workspace.lints.rust] корневого Cargo.toml.
#[cfg(kani)]
mod kani_proofs;
