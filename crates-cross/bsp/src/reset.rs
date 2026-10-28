//! Сброс МК: программный — рычаг платы, им приложение заканчивает панику{% if ota == "true" %}, а
//! узел OTA выполняет команду `Reboot` (порт `ports::Reboot`){% endif %}; причина
//! прошлого — флаги `RCC`, их читает и снимает [`take_cause`] по вызову `main`.
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

/// Причина последнего сброса по флагам `RCC.{{ reset_reg | upcase }}` — имя в
/// `snake_case` (`pin`, `software`, `independent_watchdog`…); флаги снимает
/// (`RMVF`), иначе они копились бы от сброса к сбросу.
///
/// Зовёт его один раз `main` приложения, первым делом — до `Board::new`, больше
/// никто{% if ota == "true" %}: bootloader флаги не трогает, и до приложения они доходят нетронутыми{% endif %}.
/// Второе чтение за запуск увидело бы снятые флаги (`unknown`). Флагов бывает
/// несколько сразу — внутренний сброс выставляет и флаг вывода `NRST`, —
/// поэтому проверяются от самого конкретного: выход из low-power, оконный
/// сторож, независимый сторож, программный сброс, option bytes, питание; вывод
/// — последним. Поля регистра у семейств разные; набор под этот чип выписан
/// при генерации из `stm32-metapac`. Самое длинное имя —
/// `independent_watchdog`, 20 байт.
#[must_use]
pub fn take_cause() -> &'static str {
    let rcc = embassy_stm32::pac::RCC;
    let flags = rcc.{{ reset_reg }}().read();
    rcc.{{ reset_reg }}().modify(|w| w.set_rmvf(true));
{%- if watchdog_peripheral == "IWDG2" %}
    // Прошивка второго ядра (CM4) двухъядерного H7: его сброс отмечают
    // свои флаги (RM0399, `RCC_RSR`: `LPWR2RSTF` 31, `WWDG2RSTF` 29,
    // `IWDG2RSTF` 27, `SFT2RSTF` 25), а без них сторож и `sys_reset` этого
    // ядра читались бы как вывод. У `stm32-metapac` полей для них нет —
    // сырые биты; проверяются первыми, раз ядро — своё.
    let core2 = [
        (31, "low_power"),
        (29, "window_watchdog"),
        (27, "independent_watchdog"),
        (25, "software"),
    ];
    if let Some((_, cause)) = core2
        .into_iter()
        .find(|&(bit, _)| flags.0 & (1 << bit) != 0)
    {
        return cause;
    }
{%- endif %}
{%- assign pairs = reset_flags | split: " " %}
{%- for pair in pairs %}
{%- assign p = pair | split: ":" %}
{%- capture name %}{% case p[1] %}{% when "LowPower" %}low_power{% when "WindowWatchdog" %}window_watchdog{% when "IndependentWatchdog" %}independent_watchdog{% when "Software" %}software{% when "OptionBytes" %}option_bytes{% when "PowerOn" %}power_on{% when "Pin" %}pin{% endcase %}{% endcapture %}
    {% unless forloop.first %}} else {% endunless %}if flags.{{ p[0] | split: "," | join: "() || flags." }}() {
        "{{ name }}"
{%- endfor %}
    } else {
        "unknown"
    }
}
