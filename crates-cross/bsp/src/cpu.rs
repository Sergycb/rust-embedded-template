//! Ядро Cortex-M для `app`: точка входа, сон исполнителя, сброс.
//!
//! `app` не зависит ни от `cortex-m`, ни от `cortex-m-rt`: всё, что ему нужно
//! от ядра, проходит здесь.

/// Точка входа прошивки — `#[bsp::cpu::entry]` над `main`.
pub use cortex_m_rt::entry;

/// Спит до события (`WFE`): пробуждение задачи, `SEV`, прерывание.
pub fn wait_for_event() {
    cortex_m::asm::wfe();
}

/// Запрещает прерывания — навсегда, обратного вызова нет: нужен паникёру.
pub fn disable_interrupts() {
    cortex_m::interrupt::disable();
}

/// Программный сброс МК.
pub fn reset() -> ! {
    cortex_m::peripheral::SCB::sys_reset()
}
