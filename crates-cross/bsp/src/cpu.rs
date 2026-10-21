//! Ядро Cortex-M для `app`: точка входа, сон исполнителя, сброс{% if ota == "true" %} — и порт
//! перезагрузки для узла OTA{% endif %}.
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
{%- if cache contains "scb_d" %}
///
/// Перед сбросом кэш данных ядра M7 сбрасывается в RAM (`clean`): иначе дамп паники
/// в регионе `PANIC` (он лежит в кэшируемой RAM — на F7 и H7) остался бы в кэше и пропал.
{%- endif %}
pub fn reset() -> ! {
{%- if cache contains "scb_d" %}
    clean_dcache();
{%- endif %}
    cortex_m::peripheral::SCB::sys_reset()
}
{%- if ota == "true" and ota_store == "external" %}

/// Блокирующая задержка на `SysTick` ядра — `DelayNs` драйвера внешней флеши в
/// bootloader'е: исполнителя и драйвера времени там нет. Счётчик `SysTick`
/// включается только на время задержки, и прыжок в приложение застаёт его
/// выключенным.
pub type Delay = cortex_m::delay::Delay;

/// Задержка [`Delay`] на частоте ядра `sys_hz`.
///
/// # Panics
///
/// Если периферию ядра уже кто-то взял (`cortex_m::Peripherals::take`): `SysTick`
/// у задержки один владелец, и зовётся она один раз — из bootloader'а.
#[must_use]
pub fn delay(sys_hz: u32) -> Delay {
    let core = cortex_m::Peripherals::take().expect("bsp::cpu::delay: периферия ядра уже взята");
    Delay::new(core.SYST, sys_hz)
}
{%- endif %}
{%- if ota == "true" %}

/// Перезагрузка МК для узла OTA — порт `ports::Reboot`: узел выполняет им
/// команду `Reboot` после принятого образа, и bootloader меняет разделы.
#[derive(Debug, Default, Clone, Copy)]
pub struct Reboot;

impl ports::Reboot for Reboot {
    fn reboot(&mut self) -> ! {
        reset()
    }
}
{%- endif %}
{%- if cache contains "scb_d" %}

#[expect(
    unsafe_code,
    reason = "периферия ядра берётся через `steal()`: сброс — последнее, что выполняет прошивка"
)]
fn clean_dcache() {
    // SAFETY: зовётся только из `reset`, после него ничего не выполняется.
    let mut core = unsafe { cortex_m::Peripherals::steal() };
    core.SCB.clean_dcache(&mut core.CPUID);
}
{%- endif %}
