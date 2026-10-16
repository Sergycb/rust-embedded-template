//! Кадр шины и сообщение стека `zencan`: перевод в обе стороны и заплатки
//! под отступления стека от CiA 301.

use embedded_can::{ExtendedId, Frame, Id, StandardId};
use zencan_node::common::messages::{CanId, CanMessage};

/// Кадр heartbeat этого узла: `0x700 + адрес`, один байт состояния.
pub(super) fn is_heartbeat(message: &CanMessage, node_id: u8) -> bool {
    message.id() == CanId::std(0x700 + u16::from(node_id)) && message.dlc == 1
}

/// Сигнатура `save` в SDO-записи `0x1010:01` — в том порядке байтов, которого
/// ждёт стек.
///
/// CiA 301: сигнатура — ASCII `save`, в кадре байты `73 61 76 65`, то есть
/// `u32` `0x6576_6173`. `zencan-common` 0.0.5 сравнивает с `0x7361_7665`
/// (`SAVE_CMD`) — байты наоборот, — и `save` от стандартного мастера
/// отвергает (`IncompatibleParameter`). Узел переставляет их до стека. Убрать,
/// когда исправят в zencan.
pub(super) fn fix_save_signature(message: &mut CanMessage) {
    /// Индекс `0x1010` младшим байтом первым и подындекс 1.
    const STORE_OBJECT: [u8; 3] = [0x10, 0x10, 0x01];
    const SAVE: [u8; 4] = *b"save";
    let CanId::Std(id) = message.id() else {
        return;
    };
    // Expedited-запись: cs 0x23 — с размером (4 байта), 0x22 — без него.
    if (0x601..=0x67F).contains(&id)
        && message.dlc == 8
        && matches!(message.data[0], 0x22 | 0x23)
        && message.data[1..4] == STORE_OBJECT
        && message.data[4..8] == SAVE
    {
        message.data[4..8].reverse();
    }
}

/// Кадр шины → сообщение стека; `None` — кадр CAN FD длиннее 8 байт, стек
/// классический. DLC удалённого кадра стек 0.0.5 не хранит (`new_rtr`).
pub(super) fn to_message(frame: &impl Frame) -> Option<CanMessage> {
    let id = match frame.id() {
        Id::Standard(id) => CanId::std(id.as_raw()),
        Id::Extended(id) => CanId::extended(id.as_raw()),
    };
    if frame.is_remote_frame() {
        Some(CanMessage::new_rtr(id))
    } else if frame.data().len() <= 8 {
        Some(CanMessage::new(id, frame.data()))
    } else {
        None
    }
}

/// Сообщение стека → кадр шины; `None` — идентификатор вне диапазона.
pub(super) fn to_frame<F: Frame>(message: &CanMessage) -> Option<F> {
    let id: Id = match message.id() {
        CanId::Std(id) => StandardId::new(id)?.into(),
        CanId::Extended(id) => ExtendedId::new(id)?.into(),
    };
    if message.is_rtr() {
        F::new_remote(id, usize::from(message.dlc))
    } else {
        F::new(id, message.data())
    }
}
