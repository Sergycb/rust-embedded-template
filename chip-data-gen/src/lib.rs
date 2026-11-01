//! Генератор таблиц `chip-data/`, которые `chip-select.rhai` подключает через
//! `import`, — и то, что с ним делят `template-check` и тесты каскада: корень
//! репозитория, адрес rust-lib, штамп версий. Что и когда перегенерировать —
//! MAINTAINING.md, «Когда перегенерировать данные».

pub mod emit;
pub mod generate;
pub mod source;

mod layout;
mod metadata;
