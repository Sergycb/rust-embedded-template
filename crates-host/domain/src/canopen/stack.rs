//! Стек `zencan` в кадре задачи и такт его обработки.

use core::cell::RefCell;

use embassy_time::{Duration, Instant};
use zencan_node::Node;

use super::Persisted;

/// Такт обработки: как часто узел зовёт `Node::process`, даже если кадров нет.
///
/// На такте держатся heartbeat (период обычно от 100 мс), таймауты SDO и
/// периодические TPDO; 10 мс — шаг, на который они могут опоздать. Чаще —
/// МК будится без дела, реже — heartbeat с периодом 100 мс уходит с
/// ощутимым дрожанием.
pub const TICK: Duration = Duration::from_millis(10);

/// Стек CANopen с тем, на чём он держится: колбэки заимствуют буфер
/// [`Persisted`] из кадра задачи на `'f`.
#[expect(
    missing_debug_implementations,
    reason = "типы zencan-node не реализуют Debug"
)]
pub struct Stack<'f> {
    /// Сам стек — в кадре задачи ([`task`](super::task)): состояния держат
    /// ссылку, и переход не копирует его.
    pub(super) node: &'f mut Node<'f>,
    /// Момент создания стека — начало его отсчёта времени.
    pub(super) start: Instant,
    pub(super) persisted: &'f RefCell<Persisted>,
}
