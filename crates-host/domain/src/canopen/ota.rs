//! Объект OTA в словаре CANopen: команды обновления — записями SDO.
//!
//! Объект [`OTA_INDEX`] (запись, `application_callback` в `device.toml`):
//!
//! | Sub | Доступ | Что |
//! |---|---|---|
//! | 1 | wo, domain | `Start`: длина (u32) и, если есть, подпись — 64 байта |
//! | 2 | wo, domain | `Chunk`: смещение (u32), затем байты, до 128 |
//! | 3 | wo, u32 | `Finish`: 4 байта, значение не важно |
//! | 4 | wo, u32 | `Reboot`: 4 байта, значение не важно |
//! | 5 | ro, u16 | код исхода последней команды (`domain::ota::command::reply_code`); 0 — ещё выполняется |
//! | 6 | ro, u32 | сколько байт образа принято |
//!
//! Числа — младшим байтом первым, как всё в CANopen.
//!
//! Запись SDO не ждёт исхода: `zencan`
//! отвечает на неё синхронно, внутри обработки кадра, и отложить ответ нельзя.
//! Поэтому запись только ставит команду, а узел CANopen после обработки кадра
//! (`step`) отправляет её на вход `ASK` узла OTA, не дожидаясь исхода, и
//! работает дальше: heartbeat, SDO и PDO идут и пока узел OTA стирает раздел.
//! Исход узел забирает из `reply()` и кладёт в sub 5. Хост после каждой
//! команды перечитывает sub 5, пока там 0. Новая команда до исхода прежней —
//! отказ записи `CantStoreDeviceState`: квота узла — одна команда в полёте.
//!
//! Объект — [`OTA`], `static` здесь, а не у compose-site: стек держит
//! обработчик ссылкой `'static`, а сам объект — логика, не железо. Он один на
//! прошивку, как и словарь `zencan`, в котором стоит.

use defmt_or_log::warn;
use zencan_node::common::AtomicCell;
use zencan_node::common::objects::{AccessType, DataType, ObjectCode, PdoMappable, SubInfo};
use zencan_node::common::sdo::AbortCode;
use zencan_node::object_dict::ObjectAccess;

use crate::ota::command::{CHUNK_BYTES, Chunk, Progress, Rejection, SIGNATURE_BYTES, reply_code};
use crate::ota::{ClientId, Command, Reply, Request};

/// Индекс объекта OTA — в области приложения `zencan` (0x2000–0x4FFF). Тот же,
/// что в `device.toml`; разойтись им не даёт тест узла.
pub const OTA_INDEX: u16 = 0x4F00;

/// Длина `Start` без подписи: длина образа.
const START_BYTES: usize = 4;

/// Длина заголовка `Chunk`: смещение.
const CHUNK_HEADER: usize = 4;

/// Клиент входа `ASK` узла OTA — поле `ota_ask` контекста узла CANopen.
pub(crate) type Asker<'a> = supervisor::Asker<'a, Request, Reply, ()>;

/// Объект OTA словаря: ставит команды и показывает исход последней.
///
/// Состояние — под критической секцией (`AtomicCell`): объект — `static`, а
/// стек зовёт его синхронно из `Node::process`.
pub struct Object {
    /// Команда, которую узел CANopen ещё не отдал узлу OTA.
    pending: AtomicCell<Option<Command>>,
    /// Команда поставлена, а исхода ещё нет.
    in_flight: AtomicCell<bool>,
    /// Код исхода последней команды; 0 — выполняется или команд не было.
    status: AtomicCell<u16>,
    /// Сколько байт образа принято — по последнему `Received`.
    received: AtomicCell<u32>,
}

impl Object {
    /// Объект без команд и исходов.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            pending: AtomicCell::new(None),
            in_flight: AtomicCell::new(false),
            status: AtomicCell::new(0),
            received: AtomicCell::new(0),
        }
    }

    /// Новый запуск узла: команды прежнего забыты. Свежий `Asker` бросает
    /// всё, что прежний оставил в полёте, и его исход сюда не придёт — без
    /// сброса объект отвергал бы каждую новую команду.
    pub(crate) fn reset(&self) {
        self.pending.store(None);
        self.in_flight.store(false);
    }

    /// Ждущую команду — на вход `ASK` узла OTA от клиента `client`, не
    /// дожидаясь исхода: его узел CANopen заберёт из `ota.reply()` и отдаст
    /// [`record`](Self::record). Зовёт узел после обработки кадра; нет
    /// команды — ничего.
    pub(crate) fn send(&self, ota: &mut Asker<'_>, client: ClientId) {
        let Some(command) = self.pending.take() else {
            return;
        };
        if ota.try_call((), Request { client, command }).is_err() {
            // Не бывает: запись не ставит команду, пока прежняя в полёте, а
            // квота — одна команда.
            warn!("canopen: вход ASK узла OTA занят");
            self.record(Err(Rejection::Busy));
        }
    }

    /// Исход — в sub 5 и 6; следующая команда снова принимается.
    pub(crate) fn record(&self, reply: Reply) {
        match reply {
            Ok(Progress::Received(received)) => self.received.store(received),
            // Отказ обрывает приём: длина прошлого приёма больше ни к чему.
            Ok(Progress::Started) | Err(_) => self.received.store(0),
            Ok(Progress::Ready | Progress::Rebooting) => {}
        }
        self.status.store(reply_code(&reply));
        self.in_flight.store(false);
    }

    /// Ставит команду; пока прежняя без исхода — отказ записи.
    fn post(&self, command: Command) -> Result<(), AbortCode> {
        if self.in_flight.load() {
            return Err(AbortCode::CantStoreDeviceState);
        }
        self.in_flight.store(true);
        self.status.store(0);
        self.pending.store(Some(command));
        Ok(())
    }
}

impl Default for Object {
    fn default() -> Self {
        Self::new()
    }
}

impl core::fmt::Debug for Object {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Object")
            .field("status", &self.status.load())
            .field("received", &self.received.load())
            .finish_non_exhaustive()
    }
}

impl ObjectAccess for Object {
    fn read(&self, sub: u8, offset: usize, buf: &mut [u8]) -> Result<usize, AbortCode> {
        let mut value = [0; 4];
        let value = match sub {
            0 => {
                value[0] = 6;
                &value[..1]
            }
            5 => {
                value[..2].copy_from_slice(&self.status.load().to_le_bytes());
                &value[..2]
            }
            6 => {
                value = self.received.load().to_le_bytes();
                &value[..]
            }
            1..=4 => return Err(AbortCode::WriteOnly),
            _ => return Err(AbortCode::NoSuchSubIndex),
        };
        let rest = value
            .get(offset..)
            .ok_or(AbortCode::DataTypeMismatchLengthHigh)?;
        let len = rest.len().min(buf.len());
        buf[..len].copy_from_slice(&rest[..len]);
        Ok(len)
    }

    fn read_size(&self, sub: u8) -> Result<usize, AbortCode> {
        match sub {
            0 => Ok(1),
            5 => Ok(2),
            6 => Ok(4),
            1..=4 => Ok(0),
            _ => Err(AbortCode::NoSuchSubIndex),
        }
    }

    fn write(&self, sub: u8, data: &[u8]) -> Result<(), AbortCode> {
        let command = match sub {
            1 => start(data)?,
            2 => chunk(data)?,
            3 => Command::Finish,
            4 => Command::Reboot,
            0 | 5 | 6 => return Err(AbortCode::ReadOnly),
            _ => return Err(AbortCode::NoSuchSubIndex),
        };
        self.post(command)
    }

    fn object_code(&self) -> ObjectCode {
        ObjectCode::Record
    }

    fn sub_info(&self, sub: u8) -> Result<SubInfo, AbortCode> {
        let domain = |size| SubInfo {
            size,
            data_type: DataType::Domain,
            access_type: AccessType::Wo,
            pdo_mapping: PdoMappable::None,
            persist: false,
        };
        match sub {
            0 => Ok(SubInfo::MAX_SUB_NUMBER),
            1 => Ok(domain(START_BYTES + SIGNATURE_BYTES)),
            2 => Ok(domain(CHUNK_HEADER + CHUNK_BYTES)),
            3 | 4 => Ok(SubInfo::new_u32().wo_access()),
            5 => Ok(SubInfo::new_u16().ro_access()),
            6 => Ok(SubInfo::new_u32().ro_access()),
            _ => Err(AbortCode::NoSuchSubIndex),
        }
    }
}

/// Объект OTA словаря — регистрирует его узел при старте (`canopen::begin`).
pub static OTA: Object = Object::new();

/// `Start` из записи sub 1: длина и, если есть, подпись.
fn start(data: &[u8]) -> Result<Command, AbortCode> {
    let (len, signature) = data
        .split_first_chunk::<START_BYTES>()
        .ok_or(AbortCode::DataTypeMismatchLengthLow)?;
    let signature = match signature.len() {
        0 => None,
        SIGNATURE_BYTES => signature.first_chunk::<SIGNATURE_BYTES>().copied(),
        n if n < SIGNATURE_BYTES => return Err(AbortCode::DataTypeMismatchLengthLow),
        _ => return Err(AbortCode::DataTypeMismatchLengthHigh),
    };
    Ok(Command::Start {
        len: u32::from_le_bytes(*len),
        signature,
    })
}

/// `Chunk` из записи sub 2: смещение, затем байты.
fn chunk(data: &[u8]) -> Result<Command, AbortCode> {
    let (offset, bytes) = data
        .split_first_chunk::<CHUNK_HEADER>()
        .ok_or(AbortCode::DataTypeMismatchLengthLow)?;
    Chunk::new(u32::from_le_bytes(*offset), bytes)
        .map(Command::Chunk)
        .ok_or(AbortCode::DataTypeMismatchLengthHigh)
}

#[cfg(test)]
mod tests {
    use crate::ota::Command;
    use crate::ota::command::{Chunk, Progress, Rejection};
    use zencan_node::common::sdo::AbortCode;
    use zencan_node::object_dict::ObjectAccess;

    use super::Object;

    fn read(object: &Object, sub: u8) -> Vec<u8> {
        let mut buf = [0; 8];
        let len = object.read(sub, 0, &mut buf).expect("подобъект читается");
        buf[..len].to_vec()
    }

    /// Записи ставят ровно ту команду, что описана в таблице подобъектов.
    #[test]
    fn writes_post_the_commands_of_the_table() {
        let object = Object::new();
        let mut start = 8u32.to_le_bytes().to_vec();
        start.extend([7; 64]);
        object.write(1, &start).unwrap();
        assert_eq!(
            object.pending.take(),
            Some(Command::Start {
                len: 8,
                signature: Some([7; 64])
            })
        );
        object.record(Ok(Progress::Started));

        let mut chunk = 16u32.to_le_bytes().to_vec();
        chunk.extend([1, 2, 3]);
        object.write(2, &chunk).unwrap();
        assert_eq!(
            object.pending.take(),
            Some(Command::Chunk(Chunk::new(16, &[1, 2, 3]).unwrap()))
        );
        object.record(Ok(Progress::Received(19)));

        object.write(3, &[0; 4]).unwrap();
        assert_eq!(object.pending.take(), Some(Command::Finish));
    }

    /// Пока команда без исхода — sub 5 читается нулём, исход — его кодом.
    #[test]
    fn the_status_is_zero_until_the_outcome() {
        let object = Object::new();
        object.write(3, &[0; 4]).unwrap();
        assert_eq!(read(&object, 5), [0, 0], "выполняется");

        object.pending.take();
        object.record(Err(Rejection::NoSession));
        assert_eq!(read(&object, 5), 16u16.to_le_bytes());
    }

    /// Команда до исхода прежней — отказ записи; после исхода — принимается.
    #[test]
    fn a_command_waits_for_the_outcome_of_the_previous_one() {
        let object = Object::new();
        object.write(3, &[0; 4]).unwrap();
        assert_eq!(
            object.write(4, &[0; 4]),
            Err(AbortCode::CantStoreDeviceState)
        );
        assert_eq!(object.pending.take(), Some(Command::Finish));

        object.record(Ok(Progress::Ready));
        object.write(4, &[0; 4]).unwrap();
        assert_eq!(object.pending.take(), Some(Command::Reboot));
    }

    /// Записи не по форме — отказ до постановки команды.
    #[test]
    fn malformed_writes_are_refused() {
        let object = Object::new();
        assert_eq!(
            object.write(1, &[0; 3]),
            Err(AbortCode::DataTypeMismatchLengthLow)
        );
        assert_eq!(
            object.write(1, &[0; 10]),
            Err(AbortCode::DataTypeMismatchLengthLow)
        );
        assert_eq!(
            object.write(1, &[0; 4 + 65]),
            Err(AbortCode::DataTypeMismatchLengthHigh)
        );
        assert_eq!(
            object.write(2, &[0; 2]),
            Err(AbortCode::DataTypeMismatchLengthLow)
        );
        assert_eq!(
            object.write(2, &[0; 4 + 129]),
            Err(AbortCode::DataTypeMismatchLengthHigh)
        );
        assert_eq!(object.write(5, &[0; 2]), Err(AbortCode::ReadOnly));
        assert_eq!(object.write(9, &[0; 2]), Err(AbortCode::NoSuchSubIndex));
        assert_eq!(object.pending.take(), None);
    }

    /// Подындексы по таблице модуля: размер, отказ чтения команд, отказ
    /// записи кодов исхода и чужой подындекс.
    #[test]
    fn subindices_follow_the_table() {
        let object = Object::new();
        let mut buf = [0; 8];
        for (sub, size) in [(0, Ok(1)), (1, Ok(0)), (4, Ok(0)), (5, Ok(2)), (6, Ok(4))] {
            assert_eq!(object.read_size(sub), size, "read_size({sub})");
        }
        assert_eq!(read(&object, 0), [6], "sub 0 — число подындексов");
        for sub in 1..=4 {
            assert_eq!(
                object.read(sub, 0, &mut buf),
                Err(AbortCode::WriteOnly),
                "read({sub})"
            );
        }
        for sub in [0, 5, 6] {
            assert_eq!(
                object.write(sub, &[0; 4]),
                Err(AbortCode::ReadOnly),
                "write({sub})"
            );
        }
        for sub in [7, 0xFF] {
            assert_eq!(object.read_size(sub), Err(AbortCode::NoSuchSubIndex));
            assert_eq!(
                object.read(sub, 0, &mut buf),
                Err(AbortCode::NoSuchSubIndex)
            );
            assert_eq!(object.write(sub, &[0; 4]), Err(AbortCode::NoSuchSubIndex));
        }
        assert_eq!(object.pending.take(), None);
    }

    /// Чтение со смещением: хвост значения, на конце — пусто, за концом —
    /// отказ, а не паника.
    #[test]
    fn reads_honour_the_offset() {
        let object = Object::new();
        object.record(Ok(Progress::Received(0x0403_0201)));
        let mut buf = [0; 8];
        assert_eq!(object.read(6, 1, &mut buf), Ok(3));
        assert_eq!(buf[..3], [2, 3, 4]);
        assert_eq!(object.read(6, 4, &mut buf), Ok(0));
        assert_eq!(
            object.read(6, 5, &mut buf),
            Err(AbortCode::DataTypeMismatchLengthHigh)
        );
        assert_eq!(
            object.read(5, 3, &mut buf),
            Err(AbortCode::DataTypeMismatchLengthHigh)
        );
        let mut short = [0; 2];
        assert_eq!(
            object.read(6, 0, &mut short),
            Ok(2),
            "буфер короче значения"
        );
        assert_eq!(short, [1, 2]);
    }

    /// Принятая длина — по последнему `Received`; `Started` и отказ её
    /// обнуляют, `Ready` оставляет.
    #[test]
    fn received_follows_the_replies() {
        let object = Object::new();
        object.record(Ok(Progress::Received(300)));
        assert_eq!(read(&object, 6), 300u32.to_le_bytes());
        object.record(Ok(Progress::Ready));
        assert_eq!(
            read(&object, 6),
            300u32.to_le_bytes(),
            "Ready — длина образа"
        );
        object.record(Err(Rejection::Transfer));
        assert_eq!(read(&object, 6), [0; 4]);
        object.record(Ok(Progress::Received(8)));
        object.record(Ok(Progress::Started));
        assert_eq!(read(&object, 6), [0; 4]);
    }
}
