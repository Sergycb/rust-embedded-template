//! `test-log` вместо ручного `env_logger::init()` в каждом тесте: тихо на
//! успехе, видно на провале/`--nocapture`, читает `RUST_LOG` (`cargo xtask
//! test host` уже передаёт `domain/log`). Цветной вывод уровней — фича
//! `color` крейта (включена в default), отдельный pretty-логгер
//! (`test-pretty-log`, `pretty_env_logger`) не нужен — `test-pretty-log`
//! к тому же тянет `tracing`, а не `log`, что потребовало бы моста с уже
//! используемым в проекте `defmt-or-log`.
//!
//! `#[cfg(feature = "log")]` — отдельно от общего требования domain хоть
//! какой-то из фич `log`/`defmt` (см. CLAUDE.md): без него
//! `cargo test -p domain --features defmt` (без `log`) упал бы на
//! неразрешённом `log::info!` в этом файле.

#[cfg(feature = "log")]
#[test_log::test]
fn logs_are_visible_on_failure() {
    log::info!("test-log: visible via --nocapture, on test failure, or RUST_LOG=domain=info");
    assert_eq!(2 + 2, 4);
}
