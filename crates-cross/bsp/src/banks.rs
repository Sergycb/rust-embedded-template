//! Одобанковый режим флеша на чипах, которые с завода стоят в двухбанковом
//! (G0B/G0C, G4 cat3, L4R/L4S/L4P/L4Q, L55/L56).
//!
//! Раскладка `memory.x` и фича `single-bank` у `embassy-stm32` посчитаны на
//! один непрерывный банк — так прошивка выходит наибольшей. Но режим банков —
//! это option byte, и с завода у этих семейств он двухбанковый: на G0/G4
//! `Flash::new` паникует («Embassy is configured as single-bank, but the
//! hardware is running in dual-bank mode»), а на L4+ стирание молча бьёт
//! страницы не того размера. [`ensure_single_bank`] переключает режим сама,
//! один раз на кристалл. Обоснование и порядок прошивки — docs/flash.md,
//! «Банки flash».
//!
//! Зовут его `Board::new` и `boot` — каждый первым делом, до `Flash::new`.

use embassy_stm32::pac;

/// Бит двухбанкового режима в `FLASH_OPTR` (`OpenOCD` `stm32l4x.h`, `RM0440`,
/// `RM0444`, `RM0432`, `RM0438`): `DBANK` (22) у G4 и у чипа с наибольшим
/// объёмом в серии L4+/L5, `DUAL_BANK`/`DB1M`/`DB256` (21) — у G0 и у
/// остальных. Какой именно — посчитано при генерации; чужой бит не трогается:
/// на этом чипе он может быть не определён.
const DUAL_BANK_BIT: u32 = 1 << {{dual_bank_bit}};

/// Флаги ошибок в `FLASH_SR` (сбрасываются записью единицы; остальные биты
/// зарезервированы, писать туда нельзя).
{%- if bank_switch == "l5" %}
/// У L5 — `NSSR`: `NSOPERR`, `NSPROGERR`…`NSPGSERR`, `OPTWERR`, `OPTVERR`.
const SR_ERRORS: u32 = (1 << 1) | (0x1F << 3) | (1 << 13) | (1 << 15);
{%- else %}
/// `OPERR`, `PROGERR`…`FASTERR`, `RDERR`, `OPTVERR`.
const SR_ERRORS: u32 = (1 << 1) | (0x7F << 3) | (1 << 14) | (1 << 15);
{%- endif %}

/// Ключи разблокировки `FLASH_CR` и option bytes — одни на все четыре семейства.
const KEY1: u32 = 0x4567_0123;
const KEY2: u32 = 0xCDEF_89AB;
const OPT_KEY1: u32 = 0x0819_2A3B;
const OPT_KEY2: u32 = 0x4C5D_6E7F;

{%- if bank_switch == "g0" %}

/// `BSY1`, `BSY2`, `CFGBSY` в `FLASH_SR` у G0B/G0C: у каждого банка свой флаг
/// занятости, и option bytes пишутся, только когда свободны оба и настройка.
const SR_BUSY: u32 = 0b111 << 16;
{%- elsif bank_switch != "l5" %}

/// `BSY` в `FLASH_SR`.
const SR_BUSY: u32 = 1 << 16;
{%- endif %}

/// Переводит флеш в одобанковый режим, если он ещё двухбанковый.
///
/// Звать до `Flash::new` (и вообще до любой работы с флешем) — первым делом
/// после старта. Если бит уже сброшен — выходит сразу, флеш не трогает.
///
/// Иначе пишет option byte и запускает его загрузку (`OBL_LAUNCH`), а та
/// сбрасывает МК: до возврата дело не доходит. В одобанковом режиме контроллер
/// читает оба массива 128-битным словом вперемешку, и образ, залитый в
/// двухбанковом, по тем же адресам становится мусором — поэтому новый кристалл
/// прошивается дважды: первая прошивка переключает режим, вторая ложится уже в
/// одобанковый флеш и работает. Дальше — как обычно, переключение больше не
/// срабатывает.
///
/// # Panics
///
/// Если ключи не отперли контроллер, запись отвергнута (флаг ошибки в `SR`)
/// или бит не читается сброшенным. До сброса доходит только проверенная
/// запись: иначе он повторялся бы на каждом старте, каждый раз с новой
/// попыткой, — а запертый контроллер молча игнорирует запись, флагов не ставя.
pub fn ensure_single_bank() {
    let flash = pac::FLASH;
    if flash.optr().read().0 & DUAL_BANK_BIT == 0 {
        return;
    }
    defmt::warn!(
        "флеш в двухбанковом режиме — переключаю в одобанковый и перезагружаюсь; после сброса прошейте плату ещё раз"
    );

    {%- if bank_switch == "l5" %}
    while flash.nssr().read().nsbsy() {}
    flash.nssr().write_value(pac::flash::regs::Nssr(SR_ERRORS));
    if flash.nscr().read().nslock() {
        flash.nskeyr().write_value(KEY1);
        flash.nskeyr().write_value(KEY2);
    }
    if flash.nscr().read().optlock() {
        flash.optkeyr().write_value(OPT_KEY1);
        flash.optkeyr().write_value(OPT_KEY2);
    }
    assert!(
        !flash.nscr().read().nslock() && !flash.nscr().read().optlock(),
        "ключи не отперли контроллер флеша"
    );
    flash.optr().modify(|w| w.0 &= !DUAL_BANK_BIT);
    flash.nscr().modify(|w| w.set_optstrt(true));
    while flash.nssr().read().nsbsy() {}
    let errors = flash.nssr().read().0 & SR_ERRORS;
    {%- else %}
    while flash.sr().read().0 & SR_BUSY != 0 {}
    flash.sr().write_value(pac::flash::regs::Sr(SR_ERRORS));
    if flash.cr().read().lock() {
        flash.keyr().write_value(KEY1);
        flash.keyr().write_value(KEY2);
    }
    if flash.cr().read().optlock() {
        flash.optkeyr().write_value(OPT_KEY1);
        flash.optkeyr().write_value(OPT_KEY2);
    }
    assert!(
        !flash.cr().read().lock() && !flash.cr().read().optlock(),
        "ключи не отперли контроллер флеша"
    );
    flash.optr().modify(|w| w.0 &= !DUAL_BANK_BIT);
    flash.cr().modify(|w| w.set_optstrt(true));
    while flash.sr().read().0 & SR_BUSY != 0 {}
    let errors = flash.sr().read().0 & SR_ERRORS;
    {%- endif %}

    // Без числа в сообщении: форматирование тянуло бы `core::fmt` в зону
    // bootloader'а; флаги — строкой defmt.
    if errors != 0 {
        defmt::error!("запись option bytes отвергнута, SR = {=u32:#x}", errors);
        panic!("запись option bytes отвергнута");
    }
    // `OPTR` до загрузки показывает записанное: бит на месте — запись не дошла.
    assert!(
        flash.optr().read().0 & DUAL_BANK_BIT == 0,
        "бит двухбанкового режима не сбросился в OPTR"
    );
    // Загрузка option bytes — это сброс МК: дальше этой строки код не идёт.
    {%- if bank_switch == "l5" %}
    flash.nscr().modify(|w| w.set_obl_launch(true));
    {%- else %}
    flash.cr().modify(|w| w.set_obl_launch(true));
    {%- endif %}
    cortex_m::peripheral::SCB::sys_reset()
}
