#![doc = include_str!("../../../docs/modules/bsp-rng.md")]
{%- if net_services contains "ssh" %}

use core::cell::RefCell;
{%- endif %}

use embassy_stm32::peripherals::RNG;
use embassy_stm32::{bind_interrupts, rng};
{%- if net_services contains "ssh" %}
use embassy_sync::blocking_mutex::Mutex;
use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
{%- endif %}

bind_interrupts!(struct Irqs {
    {{rng_irq}} => rng::InterruptHandler<RNG>;
});
{%- if net_services contains "ssh" %}

/// Сколько байт выдаётся под одной критической секцией. Блок отдаёт слово
/// за десятки тактов своего клока, и весь запрос под одной секцией держал бы
/// прерывания (USB, Ethernet) выключенными, пока не заполнен буфер любой длины.
const CHUNK: usize = 32;
{%- endif %}

/// Генератор случайных чисел платы. Блок включён, пока объект жив, и
/// выключается, когда его дропают: клок ему дан конфигом HAL (`config()` в
/// `bsp::board`).
type Rng = rng::Rng<'static, RNG>;
{%- if net_services contains "ssh" %}

/// Генератор платы после затравки сетевого стека — бэкенд `getrandom`.
///
/// `CriticalSectionRawMutex`, а не `NoopRawMutex`: `getrandom` — глобальная
/// функция, и откуда её позовут, решает чужой крейт. Так этот мьютекс не
/// входит в допущение «один исполнитель» (`docs/flash.md`).
static GENERATOR: Mutex<CriticalSectionRawMutex, RefCell<Option<Rng>>> =
    Mutex::new(RefCell::new(None));

/// Затравка сетевого стека — первое слово генератора; сам генератор остаётся
/// бэкендом `getrandom` — случайность для SSH. Зовёт `Board::new`, один раз.
pub(crate) fn seed(resources: crate::resources::RngResources) -> u64 {
    let mut rng = Rng::new(resources.rng, Irqs);
    let seed = rng.next_u64();
    GENERATOR.lock(|cell| *cell.borrow_mut() = Some(rng));
    seed
}

/// Бэкенд `getrandom` 0.4 (имя символа у него с 0.3 не менялось).
///
/// Крипто-крейты на устройстве берут случайность одним из двух путей, и оба
/// сходятся сюда. Одни (`sunset`) зовут `getrandom` сами и генератор
/// параметром не принимают; другие (`ml-kem`, `x25519-dalek`, `p256`) его
/// принимают — им годится `getrandom::SysRng` (`rand_core` 0.10), который ходит
/// в тот же бэкенд. У `target_os = "none"` своего источника у `getrandom` нет:
/// бэкенд выбирает `--cfg getrandom_backend="custom"` в
/// `crates-cross/.cargo/config.toml`, а реализует эта функция.
///
/// Генератора ещё нет — `UNEXPECTED`: `Board::new` ставит его до первого
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
        GENERATOR
            .lock(|cell| cell.borrow_mut().as_mut().map(|rng| rng.fill_bytes(chunk)))
            .ok_or(getrandom::Error::UNEXPECTED)?;
    }
    Ok(())
}
{%- else %}

/// Затравка сетевого стека — первое слово генератора. Блок выключается, когда
/// генератор дропается на выходе: больше он прошивке не нужен (бэкенд
/// `getrandom` над ним заводит служба `ssh`).
pub(crate) fn seed(resources: crate::resources::RngResources) -> u64 {
    Rng::new(resources.rng, Irqs).next_u64()
}
{%- endif %}
