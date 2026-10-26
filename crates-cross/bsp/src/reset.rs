//! Сброс МК: программный — рычаг платы, им приложение заканчивает панику{% if ota == "true" %}, а
//! узел OTA выполняет команду `Reboot` (порт `ports::Reboot`){% endif %}; причина
//! прошлого — флаги `RCC`, их читает и снимает [`take_cause`].
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

/// Причина последнего сброса по флагам `RCC.{{ reset_reg | upcase }}`; флаги снимает
/// (`RMVF`), иначе они копились бы от сброса к сбросу.
///
/// Зовёт его один раз `Board::new`, больше никто{% if ota == "true" %}: bootloader флаги не
/// трогает, и до приложения они доходят нетронутыми{% endif %}. Флагов бывает
/// несколько сразу — внутренний сброс выставляет и флаг вывода `NRST`, — поэтому
/// проверяются от самого конкретного (порядок вариантов `ports::ResetCause`),
/// вывод — последним. Поля регистра у семейств разные; набор под этот чип
/// выписан при генерации из `stm32-metapac`.
pub(crate) fn take_cause() -> ports::ResetCause {
    let rcc = embassy_stm32::pac::RCC;
    let flags = rcc.{{ reset_reg }}().read();
    rcc.{{ reset_reg }}().modify(|w| w.set_rmvf(true));
{%- assign pairs = reset_flags | split: " " %}
{%- for pair in pairs %}
{%- assign p = pair | split: ":" %}
    {% unless forloop.first %}} else {% endunless %}if flags.{{ p[0] | split: "," | join: "() || flags." }}() {
        ports::ResetCause::{{ p[1] }}
{%- endfor %}
    } else {
        ports::ResetCause::Unknown
    }
}
