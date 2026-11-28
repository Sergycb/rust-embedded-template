//! Паникёр бинарника: в dev — `panic-probe`, в release — дамп в переживающую
//! сброс память и сброс МК. И транспорт лога, `defmt-rtt`: у обоих ровно по
//! одному на бинарник, и оба обязаны приходить из одного места.
//!
//! Отдельным крейтом, а не модулем `app`: библиотеку `app` линкует
//! `target-tests`, где паникёр даёт `embedded-test`, и второй `#[panic_handler]`
//! в том же образе не собрался бы. Бинарники `app` подключают этот крейт
//! (`use panic_handler as _`), `target-tests` — нет. Состояние паники (что
//! читают сведения об устройстве) — в `app::panic`.
{%- if ota == "true" %}
//!
//! У bootloader'а паникёр свой, без сообщений (`crates-cross/boot`).
{%- endif %}

#![no_std]

// RTT остаётся дефолтным транспортом в обоих профилях — шаблон не знает,
// какой конкретный USART на плате пользователь отведёт под лог без пробника.
// Паттерн замены (UART, без RTT/пробника) для release — в
// docs/modules/app.md, подключается по мере готовности платы (`Board`
// должен будет отдавать рычаг UART под лог).
use defmt_rtt as _;

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
    report_and_reset(info)
}

/// Тело release-паникёра: дамп, строка в RTT, сброс.
///
/// Отдельной функцией, а не в обработчике: в dev-сборке паникёр даёт
/// `panic-probe`, и тело всё равно собрано — иначе его зависимости (`cpu`,
/// `cortex-m`, `panic-persist`) в dev-профиле оказывались бы неиспользованными.
#[cfg_attr(
    debug_assertions,
    allow(dead_code, reason = "паникёр dev-сборки — panic-probe")
)]
fn report_and_reset(info: &core::panic::PanicInfo) -> ! {
    use core::sync::atomic::{AtomicBool, Ordering};

    static PANICKING: AtomicBool = AtomicBool::new(false);

    cortex_m::interrupt::disable();
    // load/store, а не swap: на thumbv6m нет CAS, а прерывания уже выключены.
    if !PANICKING.load(Ordering::Relaxed) {
        PANICKING.store(true, Ordering::Relaxed);
        panic_persist::report_panic_info(info);
        defmt::error!("panic: {}", defmt::Display2Format(info));
    }
    cpu::System.reset_now()
}
