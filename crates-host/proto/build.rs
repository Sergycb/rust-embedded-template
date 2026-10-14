//! Генерация типов сообщений из `proto/*.proto`.
//!
//! Схему разбирает `protox` (компилятор protobuf на чистом Rust) в описание
//! `FileDescriptorSet`, а `micropb-gen` строит из него код — так системный
//! `protoc` не нужен нигде. Код ложится в `OUT_DIR`, в git его нет: схема и
//! код не могут разойтись.

use std::path::PathBuf;

use micropb_gen::{Config, Generator};

/// Схемы, из которых генерируется код.
const PROTOS: &[&str] = &["proto/host.proto"];

/// Ёмкость `Info.version`, байт: строки — `heapless::String`, и длину надо
/// знать заранее. Длиннее на проводе — сообщение не разберётся.
const VERSION_BYTES: u32 = 32;

/// Ёмкость `Error.text`, байт.
const ERROR_TEXT_BYTES: u32 = 64;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out = PathBuf::from(std::env::var("OUT_DIR")?);
    for proto in PROTOS {
        println!("cargo:rerun-if-changed={proto}");
    }

    let descriptor = protox::Compiler::new(["proto"])?
        .include_imports(true)
        .open_files(PROTOS)?
        .encode_file_descriptor_set();
    let descriptor_path = out.join("host.fdset");
    std::fs::write(&descriptor_path, descriptor)?;

    let mut generator = Generator::new();
    generator.use_container_heapless();
    generator.configure(".host.Info.version", Config::new().max_bytes(VERSION_BYTES));
    generator.configure(
        ".host.Error.text",
        Config::new().max_bytes(ERROR_TEXT_BYTES),
    );
    generator.compile_fdset_file(&descriptor_path, out.join("host.rs"))?;
    Ok(())
}
