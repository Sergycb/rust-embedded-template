#![cfg_attr(not(test), no_std)]

//! Описание устройства CANopen (`device.toml`) и словарь объектов, который
//! `zencan-build` генерирует из него (см. `build.rs`).
//!
//! Отдельным крейтом, а не модулем `domain`: генерация со своим `build.rs`
//! живёт в одном месте, и узел CANopen `domain::canopen` от описания не
//! зависит — compose-site передаёт ему `NODE_MBOX`, `NODE_STATE` и
//! `OD_TABLE` отсюда.
//!
//! Объекты приложения — `OBJECT2000`, `OBJECT2001`… (по индексу): их
//! читает и пишет логика устройства, а хост — по SDO и PDO.

/// Сгенерированный словарь объектов.
pub mod device;

pub use device::*;
