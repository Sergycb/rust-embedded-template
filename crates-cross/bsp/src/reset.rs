//! Программный сброс МК — рычаг платы: им приложение заканчивает панику{% if ota == "true" %}, а
//! узел OTA выполняет команду `Reboot` (порт `ports::Reboot`){% endif %}.
{%- if cache contains "scb_d" %}
//!
//! Рычаг, а не голый `SCB::sys_reset`: кэш данных ядра M7 перед сбросом
//! сбрасывается в RAM (`clean`), иначе дамп паники в регионе `PANIC` (он лежит
//! в кэшируемой RAM — на F7 и H7) остался бы в кэше и пропал.
{%- endif %}

/// Сброс МК.
#[derive(Debug, Default, Clone, Copy)]
pub struct Reboot;

impl Reboot {
    /// Сбрасывает МК; не возвращается.
{%- if cache contains "scb_d" %}
    #[expect(
        unsafe_code,
        reason = "периферия ядра берётся через `steal()`: сброс — последнее, что выполняет прошивка"
    )]
{%- endif %}
    pub fn reboot(self) -> ! {
{%- if cache contains "scb_d" %}
        // SAFETY: после сброса ничего не выполняется, владельцу `Peripherals`
        // конфликтовать не с кем.
        let mut core = unsafe { cortex_m::Peripherals::steal() };
        core.SCB.clean_dcache(&mut core.CPUID);
{%- endif %}
        cortex_m::peripheral::SCB::sys_reset()
    }
}
{%- if ota == "true" %}

impl ports::Reboot for Reboot {
    fn reboot(&mut self) -> ! {
        Reboot::reboot(*self)
    }
}
{%- endif %}
