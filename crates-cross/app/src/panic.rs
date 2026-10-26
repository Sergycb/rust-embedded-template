//! Паникёр приложения: в dev — `panic-probe`, в release — дамп в переживающую
//! сброс память и сброс МК.
//!
//! Здесь, а не в `bsp`: что делать с паникой — решение прошивки, а не плата.
//! {% if ota == "true" %}У bootloader'а паникёр свой, без сообщений (`crates-cross/boot`), у
//! `target-tests` — свой от `embedded-test`.{% else %}У `target-tests` паникёр свой — от `embedded-test`.{% endif %}

/// В dev-сборке паника печатается в RTT и оставляет МК в `HardFault` — под отладчиком.
#[cfg(debug_assertions)]
use panic_probe as _;

use core::cell::Cell;

use cortex_m::interrupt::Mutex;

/// Сообщение паники прошлого запуска: пишет [`take`], читает [`last`].
static LAST: Mutex<Cell<Option<&'static str>>> = Mutex::new(Cell::new(None));

/// Пишет панику в переживающий сброс регион `PANIC` и в RTT, затем сбрасывает МК.
///
/// Сначала дамп, потом лог: дамп — то, ради чего обработчик существует, и
/// ничто в печати не должно ему помешать. Сама печать не блокируется (у
/// `defmt-rtt` фича `disable-blocking-mode`): не влезла в буфер — теряется,
/// и МК всё равно сбрасывается. Флаг — от повторного входа: паника внутри самого
/// defmt (чужой `Format`, логгер уже занят) вернула бы сюда же, затёрла бы
/// первую причину и паниковала бы снова, пока не кончится стек.
#[cfg(not(debug_assertions))]
#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    use core::sync::atomic::{AtomicBool, Ordering};

    static PANICKING: AtomicBool = AtomicBool::new(false);

    cortex_m::interrupt::disable();
    // load/store, а не swap: на thumbv6m нет CAS, а прерывания уже выключены.
    if !PANICKING.load(Ordering::Relaxed) {
        PANICKING.store(true, Ordering::Relaxed);
        panic_persist::report_panic_info(info);
        defmt::error!("panic: {}", defmt::Display2Format(info));
    }
    bsp::reset::Reboot.reboot()
}

/// Читает сообщение паники прошлого запуска и запоминает его для [`last`];
/// зовёт один раз `main`, до `Board::new`. В dev — всегда `None`.
///
/// Один раз: `panic-persist` при чтении стирает свою магию, и второй вызов
/// `get_panic_message_utf8` вернул бы `None`. Строка живёт в регионе `PANIC`,
/// который до следующей паники никто не пишет, — отсюда `'static`.
pub fn take() -> Option<&'static str> {
    // Паникёр dev-сборки — `panic-probe`: дамп никто не пишет, читать нечего.
    let last = if cfg!(debug_assertions) {
        None
    } else {
        panic_persist::get_panic_message_utf8()
    };
    cortex_m::interrupt::free(|cs| LAST.borrow(cs).set(last));
    last
}
{%- if has_protobuf == "true" or net_services != "" %}

/// Сообщение, которое прочитал [`take`], — для сведений об устройстве; до
/// него — `None`.
#[must_use]
pub fn last() -> Option<&'static str> {
    cortex_m::interrupt::free(|cs| LAST.borrow(cs).get())
}
{%- endif %}
