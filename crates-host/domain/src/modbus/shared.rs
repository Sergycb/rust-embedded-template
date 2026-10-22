//! Одна карта регистров на несколько узлов Modbus — по узлу на транспорт.
//!
//! Карта проекта лежит под мьютексом [`MapMutex`] в `static` compose-site, а
//! каждый узел получает свою копию ручки [`SharedRegisters`]: запись через
//! один транспорт видна чтению через другой. Каждый вызов порта берёт
//! мьютекс на всё время вызова, поэтому два запроса с разных транспортов
//! идут по очереди, и многорегистровое значение никто не видит наполовину
//! записанным.
//!
//! Окно OTA (`OtaRegisters`) оборачивает ручку снаружи, а не лежит под
//! мьютексом: команда `Start` ждёт стирания раздела секундами, и карта на это
//! время не блокируется — её обслуживают остальные транспорты.

use core::fmt;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use ports::{Bits, BitsMut, Exception, Registers, Words, WordsMut};

/// Мьютекс карты регистров, общей у нескольких узлов.
///
/// `CriticalSectionRawMutex`, а не `NoopRawMutex` флеша и разделов OTA: с
/// ним мьютекс `Sync` и ложится в обычный `static` (`OnceLock` compose-site),
/// откуда ручку берёт каждый узел сам, без общего объекта, который граф
/// раздавал бы по узлам. Критическая секция — только на захват и отпуск,
/// а не на весь вызов карты: `await` внутри идут при открытых прерываниях.
pub type MapMutex<R> = Mutex<CriticalSectionRawMutex, R>;

/// Ручка карты регистров `R`, общей у нескольких узлов: каждый вызов порта
/// берёт мьютекс карты и отдаёт вызов ей.
pub struct SharedRegisters<R: 'static> {
    map: &'static MapMutex<R>,
}

impl<R> SharedRegisters<R> {
    /// Ручка карты `map`; копий — по одной на узел.
    #[must_use]
    pub const fn new(map: &'static MapMutex<R>) -> Self {
        Self { map }
    }
}

// Вручную, а не derive: derive потребовал бы `R: Clone`/`R: Debug`, а ручка —
// только ссылка.
impl<R> Clone for SharedRegisters<R> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<R> Copy for SharedRegisters<R> {}

impl<R> fmt::Debug for SharedRegisters<R> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SharedRegisters").finish_non_exhaustive()
    }
}

impl<R: Registers> Registers for SharedRegisters<R> {
    async fn read_coils(&mut self, start: u16, out: BitsMut<'_>) -> Result<(), Exception> {
        self.map.lock().await.read_coils(start, out).await
    }

    async fn read_discrete_inputs(
        &mut self,
        start: u16,
        out: BitsMut<'_>,
    ) -> Result<(), Exception> {
        self.map.lock().await.read_discrete_inputs(start, out).await
    }

    async fn read_holding(&mut self, start: u16, out: WordsMut<'_>) -> Result<(), Exception> {
        self.map.lock().await.read_holding(start, out).await
    }

    async fn read_input(&mut self, start: u16, out: WordsMut<'_>) -> Result<(), Exception> {
        self.map.lock().await.read_input(start, out).await
    }

    async fn write_coils(&mut self, start: u16, values: Bits<'_>) -> Result<(), Exception> {
        self.map.lock().await.write_coils(start, values).await
    }

    async fn write_holding(&mut self, start: u16, values: Words<'_>) -> Result<(), Exception> {
        self.map.lock().await.write_holding(start, values).await
    }
}

#[cfg(test)]
mod tests {
    use core::future::poll_fn;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    use ports::{Exception, Registers, Words, WordsMut};

    use super::{MapMutex, SharedRegisters};
    use crate::modbus::fakes::Map;

    /// Карта под мьютексом до конца процесса — как `static` compose-site.
    fn leak<R>(map: R) -> &'static MapMutex<R> {
        Box::leak(Box::new(MapMutex::new(map)))
    }

    /// Байты регистров `values` — старший первым, как на проводе.
    fn words(values: &[u16]) -> Vec<u8> {
        values.iter().flat_map(|v| v.to_be_bytes()).collect()
    }

    /// Готовый с первого `poll` вызов порта.
    fn now<T>(future: impl Future<Output = T>) -> T {
        let mut cx = Context::from_waker(Waker::noop());
        match pin!(future).poll(&mut cx) {
            Poll::Ready(value) => value,
            Poll::Pending => panic!("вызов карты не готов с первого poll"),
        }
    }

    /// Чтение holding-регистров `start..start + N` через `regs`.
    fn read<const N: usize>(regs: &mut impl Registers, start: u16) -> [u16; N] {
        let mut bytes = vec![0; 2 * N];
        now(regs.read_holding(start, WordsMut::new(&mut bytes))).expect("чтение карты");
        core::array::from_fn(|i| u16::from_be_bytes([bytes[2 * i], bytes[2 * i + 1]]))
    }

    /// Запись через одну ручку видна чтению через другую.
    #[test]
    fn two_handles_share_one_map() {
        let map = leak(Map::default());
        let (mut a, mut b) = (SharedRegisters::new(map), SharedRegisters::new(map));

        now(a.write_holding(2, Words::new(&words(&[7, 8])))).expect("запись карты");

        assert_eq!(read::<2>(&mut b, 2), [7, 8]);
    }

    /// Карта, запись которой ждёт между двумя регистрами — как настоящая,
    /// которая отдаёт уставку задаче или пишет во флеш.
    #[derive(Default)]
    struct Slow {
        holding: [u16; 2],
    }

    /// Один `Pending` — точка, где запись стоит наполовину.
    async fn yield_once() {
        let mut yielded = false;
        poll_fn(|cx| {
            if core::mem::replace(&mut yielded, true) {
                Poll::Ready(())
            } else {
                cx.waker().wake_by_ref();
                Poll::Pending
            }
        })
        .await;
    }

    impl Registers for Slow {
        fn read_holding(
            &mut self,
            start: u16,
            mut out: WordsMut<'_>,
        ) -> impl Future<Output = Result<(), Exception>> {
            for i in 0..out.len() {
                out.set(i, self.holding[usize::from(start + i)]);
            }
            std::future::ready(Ok(()))
        }

        async fn write_holding(&mut self, start: u16, values: Words<'_>) -> Result<(), Exception> {
            let start = usize::from(start);
            self.holding[start] = values.get(0);
            yield_once().await;
            self.holding[start + 1] = values.get(1);
            Ok(())
        }
    }

    /// Запрос с другой ручки, пришедший посреди записи, ждёт её конца и
    /// видит запись целиком.
    #[test]
    fn concurrent_requests_are_serialized() {
        let map = leak(Slow::default());
        let (mut a, mut b) = (SharedRegisters::new(map), SharedRegisters::new(map));
        let mut cx = Context::from_waker(Waker::noop());
        let value = words(&[1, 2]);
        let mut bytes = [0; 4];

        let mut write = pin!(a.write_holding(0, Words::new(&value)));
        assert!(
            write.as_mut().poll(&mut cx).is_pending(),
            "запись на полпути"
        );
        {
            let mut read = pin!(b.read_holding(0, WordsMut::new(&mut bytes)));
            assert!(
                read.as_mut().poll(&mut cx).is_pending(),
                "чтение ждёт записи"
            );
            assert_eq!(write.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
            assert_eq!(read.as_mut().poll(&mut cx), Poll::Ready(Ok(())));
        }

        assert_eq!(bytes, [0, 1, 0, 2], "запись видна целиком");
    }

    // {%- if ota == "true" %}
    /// Окно OTA снаружи ручки: пока команда ждёт узел OTA, карта свободна
    /// для остальных узлов.
    #[test]
    fn the_ota_window_does_not_lock_the_map() {
        use crate::modbus::{OTA_BASE, OtaRegisters};
        use crate::ota::tests::live;

        let map = leak(Map::default());
        // Узел OTA не опрашивается: команда ждёт его ответа сколько угодно.
        let (ota, _node) = live();
        let mut with_ota = OtaRegisters::new(SharedRegisters::new(map), ota);
        let mut other = SharedRegisters::new(map);
        let mut cx = Context::from_waker(Waker::noop());
        let start = words(&[1, 0, 9]);

        let mut command = pin!(with_ota.write_holding(OTA_BASE, Words::new(&start)));
        assert!(command.as_mut().poll(&mut cx).is_pending(), "ждёт узел OTA");

        now(other.write_holding(0, Words::new(&words(&[5])))).expect("запись карты");
        assert_eq!(read::<1>(&mut other, 0), [5]);
    }
    // {%- endif %}
}
