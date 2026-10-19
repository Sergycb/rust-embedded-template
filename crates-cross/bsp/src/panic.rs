//! Паникёр прошивки — один на `app` и `boot`: в dev — `panic-probe`, в release —
//! дамп в переживающую сброс память. `boot` собирается только в release, и ему
//! достаётся release-ветка.
//!
//! Под фичей `panic-handler`: у `target-tests` паникёр свой (`embedded-test`),
//! а `#[panic_handler]` на бинарник ровно один.

/// В dev-сборке паника печатается в RTT и оставляет МК в `HardFault` — под отладчиком.
#[cfg(debug_assertions)]
use panic_probe as _;

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

    crate::cpu::disable_interrupts();
    // load/store, а не swap: на thumbv6m нет CAS, а прерывания уже выключены.
    if !PANICKING.load(Ordering::Relaxed) {
        PANICKING.store(true, Ordering::Relaxed);
        panic_persist::report_panic_info(info);
        defmt::error!("panic: {}", defmt::Display2Format(info));
    }
    crate::cpu::reset()
}

/// Причина прошлого падения, если release-паникёр успел её записать.
///
/// Читается ровно один раз: `panic-persist` стирает свою магию, и второй вызов
/// вернёт `None`. В dev дамп никто не пишет — там и функции нет.
#[cfg(not(debug_assertions))]
#[must_use]
pub fn previous() -> Option<&'static str> {
    panic_persist::get_panic_message_utf8()
}
