//! Option byte `CPU_FREQ_BOOST` (STM32H723/725/730/733/735, RM0468).
//!
//! Без него потолок ядра на VOS0 — 520 мегагерц, с ним — 550: `embassy_stm32`
//! читает бит при `init()` и, если частота в `Config` выше потолка, падает
//! ассертом до первой строки прошивки. Шаблон тактирует ядро по умолчанию
//! (`Config::default()`, HSI), и boost ему не нужен — поэтому
//! [`ensure_cpu_freq_boost`] здесь только лежит: зовите её из `Board::new`
//! перед `embassy_stm32::init`, когда поднимете PLL выше 520 мегагерц.
//! Обоснование и проверка на плате — docs/clocks.md.

use cortex_m::peripheral::SCB;
use embassy_stm32::pac;

/// Смещения `FLASH_OPTSR2_CUR`/`PRG` от базы `FLASH` (CMSIS `stm32h723xx.h`):
/// этих регистров `stm32-metapac` не знает.
const OPTSR2_CUR: usize = 0x70;
const OPTSR2_PRG: usize = 0x74;
/// `FLASH_OPTSR2_CPUFREQ_BOOST_Pos = 2` (CMSIS `stm32h723xx.h`).
const CPUFREQ_BOOST: u32 = 1 << 2;

/// Ключи разблокировки option bytes H7 — последний байт `3B`, а не `3C`, как
/// часто цитируют по памяти.
const OPT_KEY1: u32 = 0x0819_2A3B;
const OPT_KEY2: u32 = 0x4C5D_6E7F;

/// Гарантирует `CPU_FREQ_BOOST` во flash: при первом старте на новом кристалле
/// пишет бит и перезагружает МК (бит вступает в силу только сбросом;
/// `SYSRESETREQ` достаточно, цикл питания не нужен). Дальше — сразу выходит:
/// повторная запись на каждом старте изнашивала бы flash, а в release, где
/// после паники плату перезагружает паникёр, стала бы бесконечным циклом
/// «записал → сброс», то есть сбросом посреди записи option bytes.
///
/// Звать строго ДО `embassy_stm32::init()` и до любой другой работы с flash.
/// Побочно отключает ECC на ITCM/DTCM (ST); обратно бит гасится только
/// внешним программатором.
///
/// # Panics
///
/// Если запись option bytes отвергнута (`OPTCHANGEERR`) или бит не читается
/// обратно — до сброса доходит только проверенная запись.
pub fn ensure_cpu_freq_boost() {
    if boost_in_flash() {
        return;
    }
    program_boost();
    defmt::info!("bsp: CPU_FREQ_BOOST записан в option bytes, перезагружаюсь");
    SCB::sys_reset();
}

/// Стоит ли бит во flash. Спрашивается FLASH, а не тень `SYSCFG.UR18`: SYSCFG
/// тактируется только с `embassy_stm32::init()` и до него читается нулями —
/// «boost выключен» был бы неотличим от «SYSCFG не тактируется».
fn boost_in_flash() -> bool {
    read_optsr2(OPTSR2_CUR) & CPUFREQ_BOOST != 0
}

/// Пишет бит и ждёт конца записи. На H7 option bytes отпирает только
/// `OPTKEYR`, `FLASH_KEYR` трогать не нужно.
fn program_boost() {
    let flash = pac::FLASH;
    if flash.optcr().read().optlock() {
        flash.optkeyr().write_value(OPT_KEY1);
        flash.optkeyr().write_value(OPT_KEY2);
    }
    assert!(
        !flash.optcr().read().optlock(),
        "CPU_FREQ_BOOST: ключи OPTKEYR не отперли OPTCR"
    );

    // Ошибка прошлой попытки залипает в OPTSR_CUR и иначе неотличима от свежей.
    flash.optccr().write(|w| w.set_clr_optchangeerr(true));
    // Без таймаута: одноразовое действие до старта задач, зависание здесь не
    // восстановить ничем.
    while flash.optsr_cur().read().opt_busy() {}

    // OPTSTART коммитит ВСЕ регистры `*_PRG` разом, а после сброса они держат
    // копию `*_CUR` — поэтому меняется только свой бит и только в OPTSR2_PRG.
    write_optsr2(OPTSR2_PRG, read_optsr2(OPTSR2_CUR) | CPUFREQ_BOOST);
    flash.optcr().modify(|w| w.set_optstart(true));
    while flash.optsr_cur().read().opt_busy() {}

    assert!(
        !flash.optsr_cur().read().optchangeerr(),
        "CPU_FREQ_BOOST: запись option bytes отвергнута (OPTCHANGEERR)"
    );
    assert!(
        boost_in_flash(),
        "CPU_FREQ_BOOST: запись прошла без OPTCHANGEERR, но бит не читается из OPTSR2_CUR"
    );
}

/// Адрес регистра `FLASH` по смещению — для `OPTSR2_*`, которых нет в
/// `stm32-metapac`.
fn flash_reg(offset: usize) -> *mut u32 {
    pac::FLASH.as_ptr().cast::<u8>().wrapping_add(offset).cast()
}

// Ещё один `#[expect(unsafe_code)]` в `crates-cross` (только на H723/725/730/
// 733/735): регистров `OPTSR2_*` в `stm32-metapac` нет, доступ к ним — только
// по адресу.
#[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
fn read_optsr2(offset: usize) -> u32 {
    // SAFETY: `offset` — константа модуля (0x70/0x74), адрес внутри блока
    // FLASH, выровнен на слово; чтение регистра без побочных эффектов.
    unsafe { flash_reg(offset).read_volatile() }
}

#[expect(unsafe_code, reason = "обоснование — в комментарии к `read_optsr2`")]
fn write_optsr2(offset: usize, value: u32) {
    // SAFETY: тот же адрес, что в `read_optsr2`; пишется только `OPTSR2_PRG`,
    // и только из `program_boost` при отпертом OPTCR и свободном контроллере.
    unsafe { flash_reg(offset).write_volatile(value) }
}
