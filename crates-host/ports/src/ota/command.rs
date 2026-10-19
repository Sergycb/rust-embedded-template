//! Команды узлу OTA, ответы на них и порт перезагрузки.
//!
//! Образ приходит не через свой канал, а командами от узла протокола
//! (protobuf, Modbus, CANopen — какой выбран при генерации): протокол
//! переводит свои сообщения в [`Command`](crate::OtaCommand), отдаёт их узлу
//! `domain::ota` и ждёт [`Reply`](crate::OtaReply) — исход именно этой команды,
//! который и уходит хосту. Узел OTA про протокол не знает, протокол — про флеш.

/// Самый длинный кусок образа в одной команде, байт.
///
/// Влезает в запрос любого протокола шаблона: кадр protobuf, запись Modbus
/// (FC16, до 246 байт данных) и сегментированный SDO CANopen. Длиннее кусок —
/// меньше команд, но и буфер команды в обмене между узлами растёт на столько
/// же.
pub const CHUNK_BYTES: usize = 128;

// Длина куска хранится в `u8` (см. `Chunk`): поднять предел выше — сменить и её.
const _: () = assert!(
    CHUNK_BYTES <= u8::MAX as usize,
    "длина куска не помещается в u8"
);

/// Длина подписи образа (Ed25519), байт.
pub const SIGNATURE_BYTES: usize = 64;

/// Команда узлу OTA. Узел отвечает на каждую [`Reply`].
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Command {
    /// Начать приём образа длиной `len` байт. Подпись — в начале, а не в
    /// конце: проект с подписью отвергает образ без неё ДО стирания раздела,
    /// которое уничтожило бы образ отката. Новый `Start` бросает незаконченный
    /// приём.
    Start {
        /// Длина образа, байт.
        len: u32,
        /// Подпись образа, если её передают. Проект без подписи её не читает.
        signature: Option<[u8; SIGNATURE_BYTES]>,
    },
    /// Кусок образа по своему смещению. Повтор уже принятого куска (ответ на
    /// него потерялся, хост послал снова) подтверждается без записи; разрыв
    /// или наложение — отказ `Transfer`.
    Chunk(Chunk),
    /// Образ передан целиком: сверить длину и применить — пометить обмен
    /// разделов на следующем сбросе.
    Finish,
    /// Перезагрузить МК. После принятого образа — чтобы bootloader обменял
    /// разделы; ответ уходит до сброса.
    Reboot,
}

/// Кусок образа не длиннее [`CHUNK_BYTES`] и его смещение в образе.
///
/// Массив с длиной, а не срез: команда переходит из узла протокола в узел OTA
/// по значению, и заимствовать буфер кадра протокола на это время нельзя.
///
/// Смещение — не избыточность: без него повтор куска, ответ на который
/// потерялся, лёг бы в образ второй раз, сдвинув всё, что за ним.
#[derive(Clone, PartialEq, Eq)]
pub struct Chunk {
    offset: u32,
    len: u8,
    bytes: [u8; CHUNK_BYTES],
}

impl Chunk {
    /// Кусок `data` по смещению `offset`; `None` — длиннее [`CHUNK_BYTES`].
    #[must_use]
    pub fn new(offset: u32, data: &[u8]) -> Option<Self> {
        let len = u8::try_from(data.len())
            .ok()
            .filter(|&len| usize::from(len) <= CHUNK_BYTES)?;
        let mut bytes = [0; CHUNK_BYTES];
        bytes[..data.len()].copy_from_slice(data);
        Some(Self { offset, len, bytes })
    }

    /// Смещение куска в образе, байт.
    #[must_use]
    pub const fn offset(&self) -> u32 {
        self.offset
    }

    /// Байты куска.
    #[must_use]
    pub fn as_bytes(&self) -> &[u8] {
        &self.bytes[..usize::from(self.len)]
    }
}

// Вручную, и `Debug`, и `Format`: смещение и длина, а не 128 байт образа — в
// логе они ничего не скажут, а на каждом куске раздули бы его.
impl core::fmt::Debug for Chunk {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Chunk")
            .field("offset", &self.offset)
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

#[cfg(feature = "defmt")]
impl defmt::Format for Chunk {
    fn format(&self, f: defmt::Formatter<'_>) {
        defmt::write!(f, "Chunk(offset={}, len={})", self.offset, self.len);
    }
}

/// Ответ узла OTA на одну команду.
pub type Reply = Result<Progress, Rejection>;

/// Что команда сделала.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Progress {
    /// `Start` принят: раздел стёрт под образ, ждём куски.
    Started,
    /// Кусок записан; столько байт образа принято всего.
    Received(u32),
    /// `Finish` принят: образ применён, обмен разделов — на следующем сбросе
    /// (команда [`Command::Reboot`]).
    Ready,
    /// `Reboot` принят: МК сбрасывается сразу после ответа.
    Rebooting,
}

/// Код отказа для отправителя образа.
///
/// Плоский и `Copy` намеренно: протокол переводит его в своё сообщение
/// (поле ответа, код исключения, abort SDO), а полная ошибка уходит в лог узла
/// `domain::ota`.
#[non_exhaustive]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub enum Rejection {
    /// Заявленная длина негодная: ноль, больше раздела, короче хвоста с
    /// версией.
    Length,
    /// Передача не сошлась с заявленной длиной: байт больше или меньше
    /// обещанного.
    Transfer,
    /// В разделе образ, который терять нельзя (обмен ждёт сброса или ещё не
    /// подтверждён) — сначала перезагрузка.
    Busy,
    /// Версия образа не новее текущей.
    Rollback,
    /// В `Start` нет подписи, а проект её требует. Не сошедшуюся подпись так
    /// не узнать: её проверяет bootloader после перезагрузки.
    Signature,
    /// Отказ флеша или адаптера на стороне устройства.
    Device,
    /// `Chunk` или `Finish` без приёма: `Start` не было, приём уже окончен или
    /// брошен — хост молчал дольше тайм-аута узла.
    NoSession,
}

/// Код исхода числом — общий для протоколов, которые передают его хосту
/// полем (protobuf `OtaReply.status`, входной регистр окна OTA Modbus): `1`
/// `Started`, `2` `Received`, `3` `Ready`, `4` `Rebooting`; отказы с `10`:
/// `Length`, `Transfer`, `Busy`, `Rollback`, `Signature`, `Device`,
/// `NoSession` — по порядку.
#[must_use]
pub const fn reply_code(reply: &Reply) -> u16 {
    match reply {
        Ok(Progress::Started) => 1,
        Ok(Progress::Received(_)) => 2,
        Ok(Progress::Ready) => 3,
        Ok(Progress::Rebooting) => 4,
        Err(Rejection::Length) => 10,
        Err(Rejection::Transfer) => 11,
        Err(Rejection::Busy) => 12,
        Err(Rejection::Rollback) => 13,
        Err(Rejection::Signature) => 14,
        Err(Rejection::Device) => 15,
        Err(Rejection::NoSession) => 16,
    }
}

/// Перезагрузка МК — то, чем узел OTA выполняет [`Command::Reboot`].
///
/// Порт, а не прямой сброс в `domain`: сбросить ядро умеет только плата
/// (`bsp::cpu`), а на хосте узел проверяется с фейком.
pub trait Reboot {
    /// Сбрасывает МК; не возвращается.
    fn reboot(&mut self) -> !;
}
