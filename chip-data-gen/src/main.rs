//! Перегенерирует `chip-data/` целиком:
//! `cargo run --manifest-path chip-data-gen/Cargo.toml`. Вся работа — в
//! библиотеке крейта (`chip_data_gen::generate`).

fn main() -> anyhow::Result<()> {
    chip_data_gen::generate::run()
}
