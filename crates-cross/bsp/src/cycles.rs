//! Счётчик тактов ядра — `DWT.CYCCNT`, адаптер порта `ports::CycleCounter`.
//!
//! Cortex-M3 и старше; у Cortex-M0/M0+/M23 счётчика нет, и этого модуля в
//! проекте на них нет тоже. Включается в [`Cycles::new`], то есть в
//! `Board::new`: счётчик с завода выключен, а без него `now()` читает ноль.

use cortex_m::peripheral::DWT;

/// Счётчик тактов ядра — поле `Board::cycles`.
#[derive(Debug, Clone, Copy)]
pub struct Cycles {
    hz: Option<u32>,
}

impl Cycles {
    /// Включает счётчик и запоминает частоту ядра, `hz`.
    ///
    /// Зовётся один раз, из `Board::new`. Если реализация ядра счётчика не
    /// несёт (`DWT::has_cycle_counter`), `hz()` отдаёт `None`, а `now()` —
    /// нули.
    #[must_use]
    pub fn new(hz: Option<u32>) -> Self {
        start_counter();
        Self {
            hz: hz.filter(|_| DWT::has_cycle_counter()),
        }
    }
}

impl ports::CycleCounter for Cycles {
    fn now(&self) -> u32 {
        DWT::cycle_count()
    }

    fn hz(&self) -> Option<u32> {
        self.hz
    }
}

// `cortex_m::Peripherals::take()` отдаёт периферию ядра один раз за запуск, а
// `embassy_stm32::init` на части семейств его уже занимает, поэтому `steal()`:
// флаг `take()` он не трогает. Из набора пишутся только `DCB` и `DWT`.
#[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
fn start_counter() {
    // SAFETY: зовётся из `Board::new` до запуска задач и прерываний; `DCB` и
    // `DWT` больше нигде в `bsp` не пишутся, а счётчик потом только читается.
    let mut core = unsafe { cortex_m::Peripherals::steal() };
    // На части ядер (M7) DWT заперт после включения питания. Трассировка
    // должна быть разрешена раньше счётчика, иначе запись в него может быть
    // проигнорирована.
    DWT::unlock();
    core.DCB.enable_trace();
    core.DWT.enable_cycle_counter();
}
