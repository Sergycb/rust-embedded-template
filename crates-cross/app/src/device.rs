//! Сведения об устройстве для ответа хосту на `info`{% if net_services contains "rest" %}, REST `/api/info`{% endif %}{% if net_services contains "mqtt" %}, тему MQTT `info`{% endif %}.

use core::cell::Cell;

use cortex_m::interrupt::Mutex;

/// Причина сброса этого запуска (`bsp::reset::take_cause`): пишет
/// [`set_reset`], читает [`info`].
static RESET: Mutex<Cell<&'static str>> = Mutex::new(Cell::new("unknown"));

/// Запоминает причину сброса этого запуска для [`info`]. Зовёт `main`
/// бинарника сразу за `bsp::reset::take_cause`, до `Board::new`.
pub fn set_reset(cause: &'static str) {
    cortex_m::interrupt::free(|cs| RESET.borrow(cs).set(cause));
}

/// Сведения об устройстве: версия — {% if ota == "true" %}из свойств образа `image` (те же, что в
/// баннере старта), коммит — из `shadow-rs`{% else %}и коммит — из `shadow-rs` (те же, что в
/// баннере старта){% endif %}, прошлая паника — из `panic::last`,
/// причина сброса — из [`set_reset`], остальное — с платы.
#[must_use]
pub fn info(spec: bsp::ChipSpec{% if ota == "true" %}, image: crate::version::Image{% endif %}) -> domain::device::DeviceInfo {
    domain::device::DeviceInfo {
        version: {% if ota == "true" %}image.text{% else %}crate::build::PKG_VERSION{% endif %},
        commit: crate::build::SHORT_COMMIT,
        chip: spec.chip,
        uid: spec.uid,
        flash_size: spec.flash_size,
        sys_hz: spec.sys_hz,
        reset: cortex_m::interrupt::free(|cs| RESET.borrow(cs).get()),
        last_panic: crate::panic::last()
            .map(|m| domain::device::panic_brief(m, domain::device::PANIC_BRIEF)),
    }
}
