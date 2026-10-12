//! Справочник по чипу проекта: какие пины несёт периферийный блок, с какими
//! альтернативными функциями, какие DMA-каналы к нему привязаны и от чего он
//! тактируется. Запускается как `cargo xtask pins [<БЛОК>|<ПИН>]`.
//!
//! **Чего здесь намеренно нет — генерации кода.** Распиновку проверяет
//! компилятор: `SpiSck<SPI1>` для чужого пина просто не реализован, `Peri` не
//! `Copy`, поэтому один и тот же пин нельзя отдать двум задачам, а чип-фича
//! `stm32-metapac` пакетная — пина, которого нет в корпусе, не существует и в
//! типах. Генератор распиновки из какого-нибудь `board.toml` дублировал бы
//! эти проверки и добавил второй источник правды. Компилятор не знает ровно
//! трёх вещей, ради которых справочник и написан:
//!
//! 1. какие пины вообще умеют нужный сигнал (иначе — даташит или CubeMX);
//! 2. не заняли ли вы отладочный порт или выводы кварца — типы про это молчат;
//! 3. от какого клока питается блок (нужно, когда считаешь baudrate или
//!    ловишь «периферия работает не на той частоте»).
//!
//! Данные — `stm32_metapac::metadata::METADATA`, то есть ровно тот же
//! `stm32-data`, из которого `embassy-stm32` генерирует свои типы: расхождения
//! между справочником и тем, что примет компилятор, быть не может.

use std::env;

use anyhow::bail;

mod check;
mod drift;
mod lookup;
mod notes;
mod snippet;

use check::check_resources;
use drift::warn_on_chip_drift;
use lookup::{describe, list_blocks};
use snippet::print_snippet;

fn main() -> Result<(), anyhow::Error> {
    warn_on_chip_drift();

    let mut query: Option<String> = None;
    let mut snippet = false;
    let mut check = false;
    for arg in env::args().skip(1) {
        match arg.as_str() {
            "--snippet" => snippet = true,
            "--check" => check = true,
            // Пустая строка приезжает из задачи VS Code с пустым ответом на
            // запрос аргумента — см. разбор в xtask.
            "" => {}
            flag if flag.starts_with("--") => bail!(
                "неизвестный ключ {flag}; есть только --snippet (заготовка кода под блок) и \
                 --check (проверить занятые распиновкой выводы)"
            ),
            value if query.is_none() => query = Some(value.to_owned()),
            extra => bail!("лишний аргумент {extra}: блок или вывод указывается один"),
        }
    }

    if check {
        anyhow::ensure!(
            query.is_none() && !snippet,
            "--check работает сам по себе: он смотрит не на один блок, а на всю распиновку в \
             crates-cross/bsp/src",
        );
        return check_resources();
    }

    match (query, snippet) {
        (None, false) => {
            list_blocks();
            Ok(())
        }
        (None, true) => bail!("--snippet нужен блок: cargo xtask pins USART1 --snippet"),
        (Some(query), false) => describe(&query),
        (Some(query), true) => print_snippet(&query.to_uppercase()),
    }
}
