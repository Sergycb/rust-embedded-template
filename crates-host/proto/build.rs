//! Генерация типов сообщений из `proto/*.proto`.
//!
//! Схему разбирает `protox` (компилятор protobuf на чистом Rust) в описание
//! `FileDescriptorSet`, а `micropb-gen` строит из него код — так системный
//! `protoc` не нужен нигде. Код ложится в `OUT_DIR`, в git его нет: схема и
//! код не могут разойтись.

use std::fmt::Write as _;
use std::path::PathBuf;

use micropb_gen::{Config, Generator};

/// Схемы, из которых генерируется код.
const PROTOS: &[&str] = &["proto/host.proto"];

/// Ёмкости полей переменной длины, байт: строки и `bytes` —
/// `heapless`-контейнеры, и длину надо знать заранее. Длиннее на проводе —
/// сообщение не разберётся. Имя константы крейта, поле схемы, ёмкость.
const LIMITS: &[(&str, &str, u32)] = &[
    ("VERSION_BYTES", ".host.Info.version", 32),
    ("COMMIT_BYTES", ".host.Info.commit", 16),
    ("CHIP_BYTES", ".host.Info.chip", 24),
    ("UID_BYTES", ".host.Info.uid", 12),
    // Самое длинное имя `ports::ResetCause` — `independent_watchdog`, 20.
    ("RESET_BYTES", ".host.Info.reset", 24),
    // `domain::device::PANIC_BRIEF`.
    ("PANIC_BYTES", ".host.Info.last_panic", 96),
    ("ERROR_TEXT_BYTES", ".host.Error.text", 64),
    // {%- if ota == "true" %}
    // Те же пределы, что у команды узлу OTA (`ports::SIGNATURE_BYTES`,
    // `ports::CHUNK_BYTES`): длиннее не дойдёт до узла.
    ("OTA_SIGNATURE_BYTES", ".host.OtaStart.signature", 64),
    ("OTA_CHUNK_BYTES", ".host.OtaChunk.data", 128),
    // {%- endif %}
];

/// Ёмкости повторяющихся полей (`repeated`), элементов: `heapless::Vec` с
/// ёмкостью в типе. Без строки здесь micropb-gen не соберёт схему с таким полем
/// (`Missing max_len`). Имя константы крейта, поле схемы, ёмкость.
const LENGTHS: &[(&str, &str, u32)] = &[
    // ("READINGS_LEN", ".host.Telemetry.readings", 8),
];

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
    for (_, field, bytes) in LIMITS {
        generator.configure(field, Config::new().max_bytes(*bytes));
    }
    for (_, field, len) in LENGTHS {
        generator.configure(field, Config::new().max_len(*len));
    }
    generator.compile_fdset_file(&descriptor_path, out.join("host.rs"))?;

    // Те же ёмкости — константами крейта: тот, кто кладёт в поле строку своей
    // длины (`domain::protobuf::Service` — сведения об устройстве), сверяет её на
    // этапе компиляции, а не узнаёт об обрезке на проводе.
    let mut limits = String::new();
    for (name, field, bytes) in LIMITS {
        writeln!(
            limits,
            "/// Ёмкость `{field}`, байт (`build.rs`).\npub const {name}: usize = {bytes};"
        )?;
    }
    for (name, field, len) in LENGTHS {
        writeln!(
            limits,
            "/// Ёмкость `{field}`, элементов (`build.rs`).\npub const {name}: usize = {len};"
        )?;
    }
    std::fs::write(out.join("limits.rs"), limits)?;
    Ok(())
}
