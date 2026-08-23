#![cfg_attr(not(test), no_std)]

// cfg(kani) объявлен в [workspace.lints.rust] корневого Cargo.toml.
#[cfg(kani)]
mod kani_proofs;

/// Версия прошивки и правило приёма обновления — см. модуль.
///
/// Первый настоящий модуль домена в шаблоне и заодно образец границы: `bsp`
/// достаёт четыре байта из флеша, `domain` решает, что с ними делать.
pub mod firmware;
