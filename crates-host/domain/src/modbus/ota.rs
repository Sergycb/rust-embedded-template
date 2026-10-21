//! Окно OTA в карте регистров: команды обновления — записями FC16.
//!
//! [`OtaRegisters`] оборачивает карту проекта и перехватывает только своё окно
//! с [`OTA_BASE`]; всё остальное уходит карте проекта как было. Одна запись
//! FC16 по адресу [`OTA_BASE`] — одна команда узлу OTA (`ports::OtaCommand`),
//! и ответ на запись — её исход: обычный ответ FC16 — команда выполнена,
//! исключение `ServerDeviceFailure` (04) — отвергнута. Точный код и сколько
//! байт образа принято лежат во входных регистрах (FC04) с той же базы.
//!
//! Holding-регистры с [`OTA_BASE`], одна запись на команду:
//!
//! | Команда | Регистры |
//! |---|---|
//! | `Start` | `1`, длина (старшие 16 бит), длина (младшие), подпись — 32 регистра или нет |
//! | `Chunk` | `2`, смещение (старшие), смещение (младшие), число байт, байты по два на регистр |
//! | `Finish` | `3` |
//! | `Reboot` | `4` |
//!
//! Байты — старший первым в регистре; нечётный последний — в старшем байте.
//! Входные регистры с [`OTA_BASE`]: код последнего исхода
//! (`ports::reply_code`), принято байт (старшие 16 бит), принято (младшие).
//!
//! Slave отвечает после исхода, а `Start` стирает раздел — на F2/F4/F7 это
//! секунды: тайм-аут master'а на `Start` должен быть длиннее стирания.

use defmt_or_log::warn;
use ports::{
    Bits, BitsMut, CHUNK_BYTES, Chunk, Exception, OtaCommand, Progress, Registers, SIGNATURE_BYTES,
    Words, WordsMut, reply_code,
};

use crate::ota::OtaClient;

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

/// Карта регистров проекта с окном OTA: команды обновления по [`OTA_BASE`]
/// уходят узлу OTA, остальное — карте `inner`.
pub struct OtaRegisters<R> {
    inner: R,
    /// Клиент входа `CMD` узла OTA.
    ota: OtaClient,
    /// Код последнего исхода — первый входной регистр окна.
    status: u16,
    /// Сколько байт образа принято — по последнему `Received`.
    received: u32,
}

impl<R> OtaRegisters<R> {
    /// Карта `inner` с окном OTA, команды из которого уходят `ota` — клиенту
    /// входа `CMD` узла OTA ([`OtaClient`] над `ota_cmd_requester()` графа).
    pub const fn new(inner: R, ota: OtaClient) -> Self {
        Self {
            inner,
            ota,
            status: 0,
            received: 0,
        }
    }

    /// Команда узлу OTA и её исход — ответом на запись.
    async fn command(&mut self, values: Words<'_>) -> Result<(), Exception> {
        let command = parse(values).ok_or(Exception::IllegalDataValue)?;
        let Ok(reply) = self.ota.request(command).await else {
            warn!("modbus: узел OTA не ответил");
            return Err(Exception::ServerDeviceFailure);
        };
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

// Вручную: у клиента узла OTA нет `Debug`.
impl<R: core::fmt::Debug> core::fmt::Debug for OtaRegisters<R> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("OtaRegisters")
            .field("inner", &self.inner)
            .field("status", &self.status)
            .field("received", &self.received)
            .finish_non_exhaustive()
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
fn parse(values: Words<'_>) -> Option<OtaCommand> {
    let len = values.len();
    match values.get(0) {
        1 if len == START_REGISTERS || len == START_REGISTERS + SIGNATURE_REGISTERS => {
            let signature = (len > START_REGISTERS).then(|| {
                let mut signature = [0; SIGNATURE_BYTES];
                bytes(values, START_REGISTERS, &mut signature);
                signature
            });
            Some(OtaCommand::Start {
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
            Chunk::new(word32(values, 1), data).map(OtaCommand::Chunk)
        }
        3 if len == 1 => Some(OtaCommand::Finish),
        4 if len == 1 => Some(OtaCommand::Reboot),
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

#[cfg(test)]
mod tests {
    use core::task::{Context, Poll, Waker};

    use ports::{Exception, Registers, Words, WordsMut};

    use super::{OTA_BASE, OtaRegisters};
    use crate::modbus::EmptyRegisters;
    use crate::ota::tests::{Node, live};

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

    /// Полный цикл записями FC16: каждый исход — в окне исхода.
    #[test]
    fn a_whole_update_goes_through_the_register_window() {
        let (ota, mut node) = live();
        let mut regs = OtaRegisters::new(EmptyRegisters, ota);

        assert_eq!(
            write(&mut regs, &mut node, OTA_BASE, &[1, 0, 9]),
            Ok(()),
            "Start"
        );
        assert_eq!(status(&mut regs), [1, 0, 0]);
        // 9 байт: четыре полных регистра и нечётный последний в старшем байте.
        let chunk = [2, 0, 0, 9, 0x0102, 0x0304, 0x0506, 0x0708, 0x0900];
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

    /// Запись не по форме — `IllegalDataValue`, до узла OTA она не доходит.
    #[test]
    fn a_malformed_command_is_illegal_data_value() {
        let (ota, mut node) = live();
        let mut regs = OtaRegisters::new(EmptyRegisters, ota);

        for values in [&[9][..], &[1, 0], &[2, 0, 0, 4, 1], &[3, 0]] {
            assert_eq!(
                write(&mut regs, &mut node, OTA_BASE, values),
                Err(Exception::IllegalDataValue),
                "{values:?}"
            );
        }
        assert_eq!(status(&mut regs)[0], 0, "исходов не было");
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
}
