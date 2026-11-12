//! Карта регистров устройства — то, что проект видит по шине Modbus.
//!
//! [`EmptyRegisters`] — заготовка карты проекта, [`SharedRegisters`] — ручка
//! карты, общей у нескольких узлов `MODBUS_<транспорт>`: карта лежит под
//! мьютексом [`MapMutex`] в `static` compose-site, а каждый узел получает свою
//! копию ручки, и запись через один транспорт видна чтению через другой.
//{%- if ota == "true" %}
//!
//! Окно OTA ([`OtaRegisters`]) оборачивает ручку снаружи, а не лежит под
//! мьютексом: команда `Start` ждёт стирания раздела секундами, и карта на это
//! время не блокируется — её обслуживают остальные транспорты.
//{%- endif %}

use core::fmt;

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embassy_sync::mutex::Mutex;
use ports::Exception;

use crate::modbus::{Bits, BitsMut, Registers, Words, WordsMut};
// {%- if ota == "true" %}
use defmt_or_log::warn;

use crate::ota::commands::{CHUNK_BYTES, Chunk, Progress, Rejection, SIGNATURE_BYTES, reply_code};
use crate::ota::{Client, Command};
// {%- endif %}
// {%- if ota == "true" %}

/// Начало окна OTA — высоко в адресах, чтобы не мешать карте проекта.
pub const OTA_BASE: u16 = 0xF000;

/// Сколько holding-регистров занимает окно команд: заголовок `Chunk` и кусок
/// [`CHUNK_BYTES`] по два байта на регистр.
#[expect(clippy::cast_possible_truncation, reason = "кусок — 128 байт")]
pub const OTA_REGISTERS: u16 = CHUNK_HEADER + (CHUNK_BYTES as u16).div_ceil(2);

/// Сколько входных регистров занимает окно исхода: код и принятая длина.
pub const STATUS_REGISTERS: u16 = 3;

/// Регистров перед байтами `Chunk`: код, смещение (два), число байт.
const CHUNK_HEADER: u16 = 4;

/// Регистров в `Start` без подписи: код и длина (два).
const START_REGISTERS: u16 = 3;

/// Регистров подписи в `Start`.
#[expect(clippy::cast_possible_truncation, reason = "подпись — 64 байта")]
const SIGNATURE_REGISTERS: u16 = (SIGNATURE_BYTES / 2) as u16;
// {%- endif %}

/// Карта, с которой стартуют узлы `MODBUS_<транспорт>` шаблона: таблиц нет,
/// и на любой запрос slave отвечает Illegal Function — так, как по
/// умолчанию отвечают методы [`Registers`].
///
/// Это отправная точка: проект описывает здесь свою карту — какие coils и
/// регистры видны по шине и что делать при записи (применить уставку,
/// отправить событие задаче), реализуя нужные методы трейта.
#[derive(Debug, Default, Clone, Copy)]
pub struct EmptyRegisters;

impl Registers for EmptyRegisters {}

/// Мьютекс карты регистров, общей у нескольких узлов.
///
/// `CriticalSectionRawMutex`, а не `NoopRawMutex`: с ним мьютекс `Sync` и
/// ложится в обычный `static` (`OnceLock` compose-site), откуда ручку берёт
/// каждый узел сам, без общего объекта, который приходилось бы раздавать по
/// узлам. Критическая секция — только на захват и отпуск, а не на весь вызов
/// карты: `await` внутри идут при открытых прерываниях.
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
// {%- if ota == "true" %}

/// Карта регистров проекта с окном OTA: команды обновления по [`OTA_BASE`]
/// уходят узлу OTA, остальное — карте `inner`.
///
/// Одна запись FC16 по адресу [`OTA_BASE`] — одна команда узлу OTA
/// ([`Command`]), и ответ на запись — её исход: обычный ответ FC16 — команда
/// выполнена, исключение `ServerDeviceFailure` (04) — отвергнута или узел OTA
/// не ответил (код `Device`). Точный код и сколько байт образа принято лежат во
/// входных регистрах (FC04) с той же базы; запись не по форме —
/// `IllegalDataValue` (03) и код 0.
///
/// Holding-регистры с [`OTA_BASE`], одна запись на команду:
///
/// | Команда | Регистры |
/// |---|---|
/// | `Start` | `1`, длина (старшие 16 бит), длина (младшие), подпись — 32 регистра или нет |
/// | `Chunk` | `2`, смещение (старшие), смещение (младшие), число байт, байты по два на регистр |
/// | `Finish` | `3` |
/// | `Reboot` | `4` |
///
/// Байты — старший первым в регистре; нечётный последний — в старшем байте.
/// Входные регистры с [`OTA_BASE`]: код последнего исхода
/// ([`reply_code`]), принято байт (старшие 16 бит), принято (младшие).
///
/// Slave отвечает после исхода, а `Start` стирает раздел — на F2/F4/F7 это
/// секунды: тайм-аут master'а на `Start` должен быть длиннее стирания.
#[derive(Debug)]
pub struct OtaRegisters<R> {
    inner: R,
    /// Клиент входа `CMD` узла OTA.
    ota: Client,
    /// Код последнего исхода — первый входной регистр окна.
    status: u16,
    /// Сколько байт образа принято — по последнему `Received`.
    received: u32,
}

impl<R> OtaRegisters<R> {
    /// Карта `inner` с окном OTA, команды из которого уходят `ota` — клиенту
    /// входа `CMD` узла OTA ([`Client`]).
    pub const fn new(inner: R, ota: Client) -> Self {
        Self {
            inner,
            ota,
            status: 0,
            received: 0,
        }
    }

    /// Команда узлу OTA и её исход — ответом на запись.
    async fn command(&mut self, values: Words<'_>) -> Result<(), Exception> {
        // Исход прошлой команды к этой не относится: запись не по форме
        // оставляет 0 — «исхода нет», а не чужой код.
        self.status = 0;
        let command = parse(values).ok_or(Exception::IllegalDataValue)?;
        let reply = self.ota.request(command).await.unwrap_or_else(|_lost| {
            warn!("modbus: узел OTA не ответил");
            Err(Rejection::Device)
        });
        self.status = reply_code(&reply);
        match reply {
            Ok(Progress::Received(received)) => self.received = received,
            // Отказ обрывает приём: длина прошлого приёма больше ни к чему.
            Ok(Progress::Started) | Err(_) => self.received = 0,
            Ok(Progress::Ready | Progress::Rebooting) => {}
        }
        // Код отказа — во входном регистре окна, исключение одно на все.
        match reply {
            Ok(_) => Ok(()),
            Err(_) => Err(Exception::ServerDeviceFailure),
        }
    }
}

impl<R: Registers> Registers for OtaRegisters<R> {
    async fn read_coils(&mut self, start: u16, out: BitsMut<'_>) -> Result<(), Exception> {
        self.inner.read_coils(start, out).await
    }

    async fn read_discrete_inputs(
        &mut self,
        start: u16,
        out: BitsMut<'_>,
    ) -> Result<(), Exception> {
        self.inner.read_discrete_inputs(start, out).await
    }

    async fn read_holding(&mut self, start: u16, out: WordsMut<'_>) -> Result<(), Exception> {
        match window(start, out.len(), OTA_REGISTERS) {
            // Окно команд только на запись: читать там нечего.
            Window::Inside | Window::Overlap => Err(Exception::IllegalDataAddress),
            Window::Outside => self.inner.read_holding(start, out).await,
        }
    }

    async fn read_input(&mut self, start: u16, mut out: WordsMut<'_>) -> Result<(), Exception> {
        match window(start, out.len(), STATUS_REGISTERS) {
            Window::Inside => {
                let [a, b, c, d] = self.received.to_be_bytes();
                let status = [
                    self.status,
                    u16::from_be_bytes([a, b]),
                    u16::from_be_bytes([c, d]),
                ];
                for i in 0..out.len() {
                    out.set(i, status[usize::from(start - OTA_BASE + i)]);
                }
                Ok(())
            }
            Window::Overlap => Err(Exception::IllegalDataAddress),
            Window::Outside => self.inner.read_input(start, out).await,
        }
    }

    async fn write_coils(&mut self, start: u16, values: Bits<'_>) -> Result<(), Exception> {
        self.inner.write_coils(start, values).await
    }

    async fn write_holding(&mut self, start: u16, values: Words<'_>) -> Result<(), Exception> {
        match window(start, values.len(), OTA_REGISTERS) {
            // Команда — одной записью с начала окна, а не кусками регистров.
            Window::Inside if start == OTA_BASE => self.command(values).await,
            Window::Inside | Window::Overlap => Err(Exception::IllegalDataAddress),
            Window::Outside => self.inner.write_holding(start, values).await,
        }
    }
}

/// Где диапазон запроса относительно окна OTA.
enum Window {
    Inside,
    Overlap,
    Outside,
}

/// Диапазон `start..start + len` против окна `OTA_BASE..OTA_BASE + size`.
fn window(start: u16, len: u16, size: u16) -> Window {
    let (start, end) = (u32::from(start), u32::from(start) + u32::from(len));
    let (base, top) = (u32::from(OTA_BASE), u32::from(OTA_BASE) + u32::from(size));
    if end <= base || start >= top {
        Window::Outside
    } else if start >= base && end <= top {
        Window::Inside
    } else {
        Window::Overlap
    }
}

/// Команда из записи по [`OTA_BASE`]; `None` — запись не по форме.
fn parse(values: Words<'_>) -> Option<Command> {
    let len = values.len();
    match values.get(0) {
        1 if len == START_REGISTERS || len == START_REGISTERS + SIGNATURE_REGISTERS => {
            let signature = (len > START_REGISTERS).then(|| {
                let mut signature = [0; SIGNATURE_BYTES];
                bytes(values, START_REGISTERS, &mut signature);
                signature
            });
            Some(Command::Start {
                len: word32(values, 1),
                signature,
            })
        }
        2 if len >= CHUNK_HEADER => {
            let count = values.get(3);
            if usize::from(count) > CHUNK_BYTES || len != CHUNK_HEADER + count.div_ceil(2) {
                return None;
            }
            let mut data = [0; CHUNK_BYTES];
            let data = &mut data[..usize::from(count)];
            bytes(values, CHUNK_HEADER, data);
            Chunk::new(word32(values, 1), data).map(Command::Chunk)
        }
        3 if len == 1 => Some(Command::Finish),
        4 if len == 1 => Some(Command::Reboot),
        _ => None,
    }
}

/// 32 бита из двух регистров с `at`: старшие первыми.
fn word32(values: Words<'_>, at: u16) -> u32 {
    (u32::from(values.get(at)) << 16) | u32::from(values.get(at + 1))
}

/// Байты `out` из регистров с `from`: старший байт регистра первым.
fn bytes(values: Words<'_>, from: u16, out: &mut [u8]) {
    for (i, pair) in (from..).zip(out.chunks_mut(2)) {
        let [high, low] = values.get(i).to_be_bytes();
        pair[0] = high;
        if let Some(second) = pair.get_mut(1) {
            *second = low;
        }
    }
}
// {%- endif %}

#[cfg(test)]
mod shared_tests {
    use core::future::poll_fn;
    use core::pin::pin;
    use core::task::{Context, Poll, Waker};

    use ports::Exception;

    use super::{MapMutex, SharedRegisters};
    use crate::modbus::fake::Map;
    use crate::modbus::{Registers, Words, WordsMut};

    /// Карта, запись которой ждёт между двумя регистрами — как настоящая,
    /// которая отдаёт уставку задаче или пишет во флеш.
    #[derive(Default)]
    struct Slow {
        holding: [u16; 2],
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
// {%- if ota == "true" %}

#[cfg(test)]
mod ota_tests {
    use core::task::{Context, Poll, Waker};

    use ports::Exception;

    use crate::ota::Command;
    use crate::ota::commands::{Progress, Rejection, reply_code};

    use super::{OTA_BASE, OTA_REGISTERS, OtaRegisters, STATUS_REGISTERS};
    use crate::modbus::{EmptyRegisters, Registers, Words, WordsMut};
    use crate::ota::tests::{LAYOUT, Node, live};

    /// Запись FC16 по `start`, пока рядом крутится узел OTA.
    fn write(
        regs: &mut OtaRegisters<EmptyRegisters>,
        node: &mut Node,
        start: u16,
        values: &[u16],
    ) -> Result<(), Exception> {
        let bytes: Vec<u8> = values.iter().flat_map(|v| v.to_be_bytes()).collect();
        let mut cx = Context::from_waker(Waker::noop());
        let mut write = core::pin::pin!(regs.write_holding(start, Words::new(&bytes)));
        for _ in 0..16 {
            if let Poll::Ready(result) = write.as_mut().poll(&mut cx) {
                return result;
            }
            assert!(
                node.as_mut().poll(&mut cx).is_pending(),
                "узел OTA не выходит"
            );
        }
        panic!("запись не завершилась");
    }

    /// Входные регистры окна исхода: код, принято (старшие), принято (младшие).
    fn status(regs: &mut OtaRegisters<EmptyRegisters>) -> [u16; 3] {
        let mut bytes = [0; 6];
        {
            let mut cx = Context::from_waker(Waker::noop());
            let mut read = core::pin::pin!(regs.read_input(OTA_BASE, WordsMut::new(&mut bytes)));
            assert!(matches!(read.as_mut().poll(&mut cx), Poll::Ready(Ok(()))));
        }
        [0, 2, 4].map(|i| u16::from_be_bytes([bytes[i], bytes[i + 1]]))
    }

    /// Чтение `len` регистров с `start`: holding (FC03) или входных (FC04).
    /// Окно отвечает сразу, без узла OTA.
    fn read(
        regs: &mut OtaRegisters<EmptyRegisters>,
        holding: bool,
        start: u16,
        len: u16,
    ) -> Result<Vec<u16>, Exception> {
        let mut bytes = vec![0; usize::from(len) * 2];
        let mut cx = Context::from_waker(Waker::noop());
        let out = WordsMut::new(&mut bytes);
        let result = if holding {
            core::pin::pin!(regs.read_holding(start, out)).poll(&mut cx)
        } else {
            core::pin::pin!(regs.read_input(start, out)).poll(&mut cx)
        };
        let Poll::Ready(result) = result else {
            panic!("чтение {start:#x}+{len} не завершилось сразу");
        };
        result.map(|()| {
            bytes
                .chunks(2)
                .map(|pair| u16::from_be_bytes([pair[0], pair[1]]))
                .collect()
        })
    }

    /// Полный цикл записями FC16: каждый исход — в окне исхода.
    #[test]
    fn a_whole_update_goes_through_the_register_window() {
        let (ota, mut node) = live();
        let mut regs = OtaRegisters::new(EmptyRegisters, ota);

        // Длина 9 и подпись — 32 регистра: без неё `Start` отвергнут.
        let mut start = vec![1, 0, 9];
        start.extend([0x0909; 32]);
        assert_eq!(
            write(&mut regs, &mut node, OTA_BASE, &start),
            Ok(()),
            "Start"
        );
        assert_eq!(status(&mut regs), [1, 0, 0]);
        // 9 байт: четыре полных регистра и нечётный последний в старшем байте;
        // байты 1..5 — отпечаток раскладки фейка, 5..9 — версия 9.8.1798, новее
        // версии узла; иначе `Finish` отвергнут.
        let [l0, l1, l2, l3] = LAYOUT.to_le_bytes().map(u16::from);
        let chunk = [
            2,
            0,
            0,
            9,
            0x0100 | l0,
            l1 << 8 | l2,
            l3 << 8 | 0x06,
            0x0708,
            0x0900,
        ];
        assert_eq!(
            write(&mut regs, &mut node, OTA_BASE, &chunk),
            Ok(()),
            "Chunk"
        );
        assert_eq!(status(&mut regs), [2, 0, 9]);
        assert_eq!(
            write(&mut regs, &mut node, OTA_BASE, &[3]),
            Ok(()),
            "Finish"
        );
        assert_eq!(status(&mut regs), [3, 0, 9]);
    }

    /// Отказ узла — исключение 04, точный код — в окне исхода.
    #[test]
    fn a_rejection_is_device_failure_with_the_code_in_the_status() {
        let (ota, mut node) = live();
        let mut regs = OtaRegisters::new(EmptyRegisters, ota);

        let finish = write(&mut regs, &mut node, OTA_BASE, &[3]);

        assert_eq!(finish, Err(Exception::ServerDeviceFailure));
        assert_eq!(status(&mut regs)[0], 16, "NoSession");
    }

    /// Запись не по форме — `IllegalDataValue`, до узла OTA она не доходит,
    /// а код прошлого исхода в окне не остаётся.
    #[test]
    fn a_malformed_command_is_illegal_data_value() {
        let (ota, mut node) = live();
        let mut regs = OtaRegisters::new(EmptyRegisters, ota);

        for values in [&[9][..], &[1, 0], &[2, 0, 0, 4, 1], &[3, 0]] {
            // Перед каждой — исход настоящей команды: `Finish` без приёма.
            assert_eq!(
                write(&mut regs, &mut node, OTA_BASE, &[3]),
                Err(Exception::ServerDeviceFailure)
            );
            assert_eq!(status(&mut regs)[0], 16, "NoSession");
            assert_eq!(
                write(&mut regs, &mut node, OTA_BASE, values),
                Err(Exception::IllegalDataValue),
                "{values:?}"
            );
            assert_eq!(status(&mut regs)[0], 0, "{values:?}: исхода нет");
        }
    }

    /// Узел OTA взял команду и не ответил — исключение 04 и код `Device`,
    /// принятая длина обнулена, а не исход прошлой команды.
    #[test]
    fn a_lost_node_is_device_failure_with_the_device_code() {
        let (ota, mut cmd) = crate::ota::tests::cmd();
        let mut node: Node = Box::pin(async move {
            let (_, served) = cmd.serve().await;
            served.respond(Ok(Progress::Received(9)));
            // Второй запрос узел берёт и бросает, не ответив: `_` — дроп сразу.
            let (request, _) = cmd.serve().await;
            assert_eq!(request.command, Command::Finish);
            core::future::pending::<()>().await;
        });
        let mut regs = OtaRegisters::new(EmptyRegisters, ota);

        assert_eq!(write(&mut regs, &mut node, OTA_BASE, &[3]), Ok(()));
        assert_eq!(status(&mut regs), [2, 0, 9]);
        assert_eq!(
            write(&mut regs, &mut node, OTA_BASE, &[3]),
            Err(Exception::ServerDeviceFailure)
        );
        assert_eq!(
            status(&mut regs),
            [reply_code(&Err(Rejection::Device)), 0, 0]
        );
    }

    /// Запись в середину окна — не команда; вне окна — карта проекта.
    #[test]
    fn addresses_inside_the_window_but_not_at_its_base_are_refused() {
        let (ota, mut node) = live();
        let mut regs = OtaRegisters::new(EmptyRegisters, ota);

        assert_eq!(
            write(&mut regs, &mut node, OTA_BASE + 1, &[1]),
            Err(Exception::IllegalDataAddress)
        );
        assert_eq!(
            write(&mut regs, &mut node, OTA_BASE - 1, &[1, 3]),
            Err(Exception::IllegalDataAddress),
            "наложение на окно"
        );
        assert_eq!(
            write(&mut regs, &mut node, 0, &[1]),
            Err(Exception::IllegalFunction),
            "вне окна — карта проекта (пустая)"
        );
    }

    /// Окно команд не читается ни целиком, ни частью, ни наложением с
    /// любого края; сразу за ним — снова карта проекта.
    #[test]
    fn the_command_window_is_not_readable() {
        let (ota, _node) = live();
        let mut regs = OtaRegisters::new(EmptyRegisters, ota);
        let top = OTA_BASE + OTA_REGISTERS;

        for (start, len) in [
            (OTA_BASE, OTA_REGISTERS),
            (OTA_BASE, 1),
            (OTA_BASE + 5, 3),
            (top - 1, 1),
            (OTA_BASE - 1, 2),
            (top - 1, 2),
        ] {
            assert_eq!(
                read(&mut regs, true, start, len),
                Err(Exception::IllegalDataAddress),
                "FC03 {start:#x}+{len}"
            );
        }
        for (start, len) in [(OTA_BASE - 1, 1), (top, 1)] {
            assert_eq!(
                read(&mut regs, true, start, len),
                Err(Exception::IllegalFunction),
                "FC03 {start:#x}+{len}: карта проекта (пустая)"
            );
        }
    }

    /// Окно исхода читается и с середины; наложение на его край — отказ.
    #[test]
    fn the_status_window_is_readable_from_any_register_in_it() {
        let (ota, mut node) = live();
        let mut regs = OtaRegisters::new(EmptyRegisters, ota);
        // Исход с ненулевым кодом: `Finish` без приёма — `NoSession`.
        assert_eq!(
            write(&mut regs, &mut node, OTA_BASE, &[3]),
            Err(Exception::ServerDeviceFailure)
        );

        assert_eq!(read(&mut regs, false, OTA_BASE + 1, 2), Ok(vec![0, 0]));
        assert_eq!(read(&mut regs, false, OTA_BASE, 1), Ok(vec![16]));
        for (start, len) in [(OTA_BASE - 1, 2), (OTA_BASE + 2, 2)] {
            assert_eq!(
                read(&mut regs, false, start, len),
                Err(Exception::IllegalDataAddress),
                "FC04 {start:#x}+{len}"
            );
        }
        assert_eq!(
            read(&mut regs, false, OTA_BASE + STATUS_REGISTERS, 1),
            Err(Exception::IllegalFunction),
            "за окном — карта проекта (пустая)"
        );
    }
}
// {%- endif %}
