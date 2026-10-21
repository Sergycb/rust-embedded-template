//! Случайные байты для `getrandom` — его бэкенд над аппаратным RNG платы.
//!
//! Крипто-крейты на устройстве берут случайность одним из двух путей, и оба
//! сходятся сюда. Одни (`sunset`) зовут `getrandom` сами и генератор
//! параметром не принимают; другие (`ml-kem`, `x25519-dalek`, `p256`) его
//! принимают — им годится `getrandom::SysRng` (`rand_core` 0.10), который ходит
//! в тот же бэкенд. У `target_os = "none"` своего источника у `getrandom` нет:
//! бэкенд выбирает `--cfg getrandom_backend="custom"` в
//! `crates-cross/.cargo/config.toml`, а реализует этот модуль
//! (`docs/modules/bsp-rng.md`). На хосте тесты берут системный.

use core::cell::RefCell;

use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;

/// Генератор платы после затравки сетевого стека.
///
/// `CriticalSectionRawMutex`, а не `NoopRawMutex`: `getrandom` — глобальная
/// функция, и откуда её позовут, решает чужой крейт. Так этот мьютекс не
/// входит в допущение «один исполнитель» (`docs/flash.md`).
static RNG: Mutex<CriticalSectionRawMutex, RefCell<Option<bsp::rng::Rng>>> =
    Mutex::new(RefCell::new(None));

/// Сколько байт выдаётся под одной критической секцией. Блок отдаёт слово
/// за десятки тактов своего клока, и весь запрос под одной секцией держал бы
/// прерывания (USB, Ethernet) выключенными, пока не заполнен буфер любой длины.
const CHUNK: usize = 32;

/// Отдаёт генератор бэкенду `getrandom`; зовёт `Parts::new`, один раз.
pub fn install(rng: bsp::rng::Rng) {
    RNG.lock(|cell| *cell.borrow_mut() = Some(rng));
}

/// Бэкенд `getrandom` 0.4 (имя символа у него с 0.3 не менялось).
///
/// Генератора ещё нет — `UNEXPECTED`: `Parts::new` ставит его до первого
/// узла, так что это ошибка сборки прошивки, а не случай в поле.
#[expect(
    unsafe_code,
    reason = "внешний символ, который ищет `getrandom`, и запись по его указателю \
              (`crates-cross/Cargo.toml`, `[workspace.lints.rust]`)"
)]
#[unsafe(no_mangle)]
unsafe extern "Rust" fn __getrandom_v03_custom(
    dest: *mut u8,
    len: usize,
) -> Result<(), getrandom::Error> {
    // SAFETY: `getrandom` передаёт `len` байт по `dest`, доступных на запись, но,
    // возможно, неинициализированных: срез над ними — только после обнуления.
    let dest = unsafe {
        core::ptr::write_bytes(dest, 0, len);
        core::slice::from_raw_parts_mut(dest, len)
    };
    for chunk in dest.chunks_mut(CHUNK) {
        RNG.lock(|cell| cell.borrow_mut().as_mut().map(|rng| rng.fill_bytes(chunk)))
            .ok_or(getrandom::Error::UNEXPECTED)?;
    }
    Ok(())
}
