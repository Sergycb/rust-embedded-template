//! Порт счётчика тактов ядра.
//!
//! Реализует `bsp::cycles::Cycles` (счётчик `DWT` Cortex-M3 и старше), отдаёт
//! полем `Board::cycles`. Нужен логике, которой требуется время короче тика
//! планировщика: длительность операции, бюджет обработчика.

/// Счётчик тактов ядра: свободно бегущий, 32 бита, оборачивается.
///
/// На 480 мегагерцах оборот — около девяти секунд, поэтому мерить им можно интервалы
/// короче оборота: разность считается по модулю 2³², и один оборот между
/// отсчётами неотличим от нуля.
#[must_use]
pub trait CycleCounter {
    /// Текущее значение счётчика.
    fn now(&self) -> u32;

    /// Частота счёта, Гц, — частота ядра; `None`, если HAL её не посчитал.
    fn hz(&self) -> Option<u32>;

    /// Тактов, прошедших с `start` (значения [`now`](Self::now)), по модулю 2³².
    fn since(&self, start: u32) -> u32 {
        self.now().wrapping_sub(start)
    }

    /// Выполняет `f` и возвращает результат и число потраченных тактов.
    ///
    /// Прерывания и вытеснение входят в замер: счётчик ядра о них не знает.
    fn measure<R>(&self, f: impl FnOnce() -> R) -> (R, u32) {
        let start = self.now();
        let result = f();
        (result, self.since(start))
    }
}

#[cfg(test)]
mod tests {
    use super::CycleCounter;
    use core::cell::Cell;

    /// Счётчик, который «тикает» на заданное число тактов при каждом чтении.
    struct Stepping {
        value: Cell<u32>,
        step: u32,
    }

    impl CycleCounter for Stepping {
        fn now(&self) -> u32 {
            let value = self.value.get();
            self.value.set(value.wrapping_add(self.step));
            value
        }

        fn hz(&self) -> Option<u32> {
            None
        }
    }

    #[test]
    fn measure_counts_cycles_between_the_reads() {
        let counter = Stepping {
            value: Cell::new(100),
            step: 7,
        };
        assert_eq!(counter.measure(|| 42), (42, 7));
    }

    #[test]
    fn the_difference_survives_a_wrap() {
        let counter = Stepping {
            value: Cell::new(u32::MAX - 2),
            step: 10,
        };
        // Первое чтение — u32::MAX - 2, второе — оборот и 7.
        assert_eq!(counter.measure(|| ()).1, 10);
    }
}
