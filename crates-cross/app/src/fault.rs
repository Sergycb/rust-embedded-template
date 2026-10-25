//! Фолты ядра: `HardFault` с причиной и ловушка деления на ноль.
//!
//! С завода любой фолт — молчаливый зацикленный `DefaultHandler`, и в release
//! плата просто зависает. Обработчик ниже превращает его в панику с регистрами
//! причины: дальше работает штатный путь паники — дамп в регион `PANIC`
//! (`cargo xtask panic-info`), лог, сброс.
//!
//! Конфигурируемые фолты (`MemManage`, `BusFault`, `UsageFault`) не включаются
//! отдельно: выключенные, они эскалируют в `HardFault`, а регистр `CFSR`
//! заполняется в любом случае — отдельные обработчики ничего не добавили бы.
//!
//! Здесь, а не в `bsp`: что делать с фолтом — решение прошивки. У `#[exception]`
//! экспортируемое имя, и с LTO обработчик попал бы в образ всякого бинарника,
//! который линкует его крейт{% if ota == "true" %}, — в том числе `boot`, где форматирование
//! регистров стоило бы около 14 KiB зоны. Bootloader и `target-tests` остаются{% else %}. `target-tests` остаётся{% endif %}
//! на `HardFault` по умолчанию из `cortex-m-rt`.

{%- if core_baseline != "true" %}
use cortex_m::peripheral::SCB;
{%- endif %}
use cortex_m_rt::{ExceptionFrame, exception};
{%- if core_baseline != "true" %}

/// `SCB.CCR.DIV_0_TRP`: целочисленное деление на ноль — `UsageFault`, а не нуль.
const DIV_0_TRP: u32 = 1 << 4;
/// `CFSR.MMARVALID`/`CFSR.BFARVALID`: `MMFAR`/`BFAR` хранят адрес именно этого фолта.
const MMARVALID: u32 = 1 << 7;
const BFARVALID: u32 = 1 << 15;

/// Включает ловушку деления на ноль. Зовётся из `main` первым делом, до
/// задач и прерываний.
///
/// Rust проверяет делитель сам, так что ловушка ловит деление в `unsafe`,
/// ассемблере и сишных библиотеках. `UNALIGN_TRP` не включается намеренно:
/// на Armv7-M компилятор выдаёт невыровненные `LDR`/`STR` как обычные, и
/// ловушка роняла бы корректный код.
#[expect(
    unsafe_code,
    reason = "регистр `CCR` — по указателю ядра, обёртки без unsafe нет"
)]
pub fn enable_div_0_trap() {
    // SAFETY: одно чтение-модификация-запись `CCR` из `main`, до задач и прерываний.
    unsafe {
        (*SCB::PTR).ccr.modify(|ccr| ccr | DIV_0_TRP);
    }
}
{%- endif %}

/// Фолт ядра → паника с причиной. Это не «ошибка в коде паники»: паникёр
/// приложения дальше сам пишет дамп и сбрасывает МК (в dev — `panic-probe`).
// `allow`, а не `expect`: линт срабатывает на коде, который раскрывает `#[exception]`, и
// `expect` над макросом остаётся неисполненным.
#[allow(
    unsafe_code,
    reason = "обработчик исключения cortex-m-rt — `unsafe fn`, регистры причины читаются по указателю ядра"
)]
#[exception]
unsafe fn HardFault(frame: &ExceptionFrame) -> ! {
{%- if core_baseline != "true" %}
    // SAFETY: регистры состояния фолта только читаются.
    let scb = unsafe { &*SCB::PTR };
    let cfsr = scb.cfsr.read();
    // Адреса верны, только пока выставлен их бит валидности.
    let mmfar = if cfsr & MMARVALID != 0 {
        scb.mmfar.read()
    } else {
        0
    };
    let bfar = if cfsr & BFARVALID != 0 {
        scb.bfar.read()
    } else {
        0
    };
    panic!(
        "HardFault pc={:#010x} lr={:#010x} cfsr={:#010x} hfsr={:#010x} mmfar={:#010x} bfar={:#010x}",
        frame.pc(),
        frame.lr(),
        cfsr,
        scb.hfsr.read(),
        mmfar,
        bfar,
    )
{%- else %}
    panic!("HardFault pc={:#010x} lr={:#010x}", frame.pc(), frame.lr())
{%- endif %}
}
