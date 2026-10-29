//! Словарь объектов, который compose-site передаёт узлу `CANOPEN` во входе.

use zencan_node::object_dict::ODEntry;
use zencan_node::{NodeMbox, NodeState};

/// Сгенерированный словарь объектов: ссылки на `static` из крейта
/// `crates-host/canopen`.
#[derive(Clone, Copy)]
#[expect(
    missing_debug_implementations,
    reason = "типы zencan-node не реализуют Debug"
)]
pub struct Dictionary {
    /// `canopen::NODE_MBOX` — очереди приёма и передачи.
    pub mbox: &'static NodeMbox,
    /// `canopen::NODE_STATE` — PDO и общее состояние узла.
    pub state: &'static NodeState<'static>,
    /// `canopen::OD_TABLE` — словарь объектов.
    pub od: &'static [ODEntry<'static>],
    /// `canopen::OBJECT1018` — идентичность: серийный номер узел ставит при
    /// старте.
    pub identity: &'static ::canopen::Object1018,
    // {%- if ota == "true" %}
    /// Место объекта OTA ([`OTA_INDEX`](super::OTA_INDEX)) в словаре —
    /// `application_callback`: обработчик [`OtaObject`](super::OtaObject) узел
    /// регистрирует при старте.
    pub ota: &'static zencan_node::object_dict::CallbackObject<'static>,
    // {%- endif %}
}

// Словарь примера `crates-host/canopen` — только в проекте с CANopen: без него
// крейта словаря в проекте нет вовсе.
/// Словарь примера устройства `crates-host/canopen` — с ним стартует узел
/// шаблона. Проект меняет `device.toml` того крейта, а не эту функцию.
#[must_use]
pub fn dictionary() -> Dictionary {
    Dictionary {
        mbox: &::canopen::NODE_MBOX,
        state: &::canopen::NODE_STATE,
        od: &::canopen::OD_TABLE,
        identity: &::canopen::OBJECT1018,
        // {%- if ota == "true" %}
        ota: &::canopen::OBJECT4F00,
        // {%- endif %}
    }
}

/// Серийный номер для объекта идентичности (0x1018:04) — 32 бита из
/// заводского номера чипа (`bsp::ChipSpec::uid`): XOR трёх его слов.
///
/// Заводской номер STM32 — 96 бит (координаты кристалла на пластине, номер
/// пластины и партии), и единственен он только целиком. Свёртка этого не
/// гарантирует: у двух кристаллов XOR слов может совпасть, — но на одной
/// шине такое маловероятно.
#[must_use]
pub fn serial(uid: [u8; 12]) -> u32 {
    let (words, _) = uid.as_chunks::<4>();
    words
        .iter()
        .fold(0, |acc, word| acc ^ u32::from_le_bytes(*word))
}
