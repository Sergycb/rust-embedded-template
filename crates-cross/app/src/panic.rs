//! Обработчик паники: в dev — `panic-probe`, в release — дамп в переживающую сброс память.

/// В dev-сборке паника печатается в RTT и оставляет МК в HardFault — под отладчиком.
#[cfg(debug_assertions)]
use panic_probe as _;

/// Пишет панику в переживающий сброс регион `PANIC` и в RTT, затем сбрасывает МК.
///
/// Сначала дамп, потом лог: после `probe-rs attach` RTT блокирует запись при
/// заполненном канале, и без пробника печать встала бы навсегда — дамп
/// должен успеть до неё. Флаг — от повторного входа: паника внутри самого
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
    cortex_m::peripheral::SCB::sys_reset()
}
