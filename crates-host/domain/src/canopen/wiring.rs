//! Вход узла `CANOPEN` и словарь объектов, который ему передаёт compose-site.

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
    // {%- if ota == "true" %}
    /// Место объекта OTA ([`OTA_INDEX`](super::OTA_INDEX)) в словаре —
    /// `application_callback`: обработчик [`OtaObject`](super::OtaObject) узел
    /// регистрирует при старте.
    pub ota: &'static zencan_node::object_dict::CallbackObject<'static>,
    // {%- endif %}
}

/// Вход узла `CANOPEN`: шина, адрес, словарь и из чего собрать хранилище
/// настроек `S`. Строит его compose-site:
///
/// ```ignore
// {%- if graph == "true" %}
/// fragments: [::domain::CANOPEN_FRAG<bsp::can::can::Can,
///                                    ::adapters::settings::Settings<bsp::ConfigFlash>> as CANOPEN
///                 = ::domain::canopen::CanOpenWiring { bus: board.can, node_id: 1,
///                       dictionary: ::domain::canopen::dictionary(),
///                       settings: board.config, persist: true,
// {%- if ota == "true" %}
///                       ota_client: ::domain::ota::ClientId(0),
// {%- endif %}
///                 }];
// {%- else %}
/// // crates-cross/app/src/tasks/canopen.rs
/// let inputs = ::domain::canopen::CanOpenWiring { bus: board.can, node_id: 1,
///     dictionary: ::domain::canopen::dictionary(),
///     settings: board.config, persist: true,
// {%- if ota == "true" %}
///     ota_client: ::domain::ota::ClientId(0),
// {%- endif %}
/// };
// {%- endif %}
/// ```
///
/// Написан руками, а не выведен фрагментом: слот хранилища инициализируется
/// сборкой (порт [`Assemble`](ports::Assemble)) над рычагом платы, а не
/// чтением поля.
pub struct CanOpenWiring<C, S: ports::Assemble> {
    /// Шина CAN платы.
    pub bus: C,
    /// Адрес узла на шине, 1..=127.
    pub node_id: u8,
    /// Словарь объектов устройства.
    pub dictionary: Dictionary,
    /// Из чего собрать хранилище: раздел настроек платы или `()` без него.
    pub settings: S::Source,
    /// Сохранять ли объекты и адрес: `false` — хранилища у проекта нет.
    pub persist: bool,
    // {%- if ota == "true" %}
    /// Номер клиента узла OTA — транспорта: с ним команды объекта OTA
    /// словаря уходят на вход `ASK` (`asks:` фрагмента).
    pub ota_client: crate::ota::ClientId,
    // {%- endif %}
}

// Вручную: печатать шину и словарь незачем, а `Debug` есть не у всех.
impl<C, S: ports::Assemble> core::fmt::Debug for CanOpenWiring<C, S> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("CanOpenWiring")
            .field("node_id", &self.node_id)
            .finish_non_exhaustive()
    }
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
        // {%- if ota == "true" %}
        ota: &::canopen::OBJECT4F00,
        // {%- endif %}
    }
}
