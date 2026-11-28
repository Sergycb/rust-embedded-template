//! Состояние паники приложения: причина прошлого запуска и причина сброса —
//! то, что читают сведения об устройстве и узел протокола.
//!
//! Сам паникёр и транспорт лога — в крейте `panic-handler`: он входит в
//! каждый бинарник пакета отдельно (`src/main.rs`{% if test_image == "true" %}, `src/bin/test-ota.rs`{% endif %}),
//! а не в библиотеку: библиотеку линкует `target-tests`, где паникёр даёт
//! `embedded-test`, и второй `#[panic_handler]` в том же образе не собрался бы.
//! Здесь же только состояние — оно нужно и библиотеке.

use core::cell::Cell;

use cortex_m::interrupt::Mutex;

/// Сообщение паники прошлого запуска: пишет [`take`], читает [`last`].
static LAST: Mutex<Cell<Option<&'static str>>> = Mutex::new(Cell::new(None));

/// Причина сброса этого запуска (`cpu::System::take_cause`): пишет
/// [`set_reset`], читает [`reset_cause`].
static RESET: Mutex<Cell<&'static str>> = Mutex::new(Cell::new("unknown"));

/// Запоминает причину сброса этого запуска для сведений об устройстве.
/// Зовёт `main` бинарника сразу за `cpu::System::take_cause`, до `cpu::init`.
pub fn set_reset(cause: &'static str) {
    cortex_m::interrupt::free(|cs| RESET.borrow(cs).set(cause));
}

/// Причина сброса, которую запомнил [`set_reset`]; до неё — `"unknown"`.
#[must_use]
pub fn reset_cause() -> &'static str {
    cortex_m::interrupt::free(|cs| RESET.borrow(cs).get())
}

/// Читает сообщение паники прошлого запуска и запоминает его для [`last`];
/// зовёт один раз `main`, до `cpu::init`. В dev — всегда `None`.
///
/// Один раз: `panic-persist` при чтении стирает свою магию, и второй вызов
/// `get_panic_message_utf8` вернул бы `None`. Строка живёт в регионе `PANIC`,
/// который до следующей паники никто не пишет, — отсюда `'static`.
#[must_use = "прочитать причину можно один раз — её печатают сразу"]
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
{%- if has_protobuf == "true" or net_services != "" or has_cli == "true" %}

/// Сообщение, которое прочитал [`take`], — для сведений об устройстве; до
/// него — `None`.
#[must_use]
pub fn last() -> Option<&'static str> {
    cortex_m::interrupt::free(|cs| LAST.borrow(cs).get())
}
{%- endif %}
