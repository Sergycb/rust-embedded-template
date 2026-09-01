//! `test-log` вместо ручного `env_logger::init()` в каждом тесте: тихо на
//! успехе, видно на провале/`--nocapture`, читает `RUST_LOG` (`cargo xtask
//! test host` уже передаёт `domain/log`). Цветной вывод уровней — фича
//! `color` крейта (включена в default), отдельный pretty-логгер
//! (`test-pretty-log`, `pretty_env_logger`) не нужен — `test-pretty-log`
//! к тому же тянет `tracing`, а не `log`, что потребовало бы моста с уже
//! используемым в проекте `defmt-or-log`.

#[test_log::test]
fn logs_are_visible_on_failure() {
    log::info!("test-log: visible via --nocapture, on test failure, or RUST_LOG=domain=info");
    assert_eq!(2 + 2, 4);
}
