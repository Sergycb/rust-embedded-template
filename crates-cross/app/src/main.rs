//! Рабочий образ: только точка входа, тело прошивки — `app::run`
//! (`src/run.rs`).
#![no_std]
#![no_main]

#[cortex_m_rt::entry]
fn main() -> ! {
    app::run()
}
