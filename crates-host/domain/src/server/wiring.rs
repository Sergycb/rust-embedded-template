//! Вход узла протокола: канал и чем отвечать.
// {%- if graph != "true" %}
//!
//! Без графа здесь же и задача узла над входом — `serve`.
// {%- endif %}

// {%- if graph != "true" %}
use defmt_or_log::FormatOrDebug;
use ports::FrameLink;

use super::{Handler, ServerCtx, task};
// {%- endif %}

/// Вход узла: канал `L` и чем отвечать.
///
// {%- if graph == "true" %}
/// Канал строит compose-site над рычагом платы в памяти, которую объявляет
/// граф (`<вид>`/`<транспорт>` — как в `crates-cross/app/src/graph.rs`):
///
/// ```ignore
/// fragments: [::domain::SERVER_FRAG<crate::link::<вид>::<транспорт>::Link, ::domain::modbus::Slave<crate::modbus::Handle>>
///                 as MODBUS_<ТРАНСПОРТ> = ::domain::server::ServerWiring {
///                     link: crate::link::<вид>::<транспорт>::link(board.<транспорт>),
///                     handler: ::domain::modbus::Slave::new(1, crate::modbus::handle(), crate::build::PKG_VERSION),
///                 }];
/// ```
// {%- else %}
/// Канал строит задача `crates-cross/app/src/tasks/modbus.rs`.
// {%- endif %}
///
/// Буфер кадра — у канала (`FrameLink::buffer`), а не у узла: его размер
/// задаёт протокол над каналом, и канал строится вместе с ним.
#[derive(Debug)]
pub struct ServerWiring<L, H> {
    /// Кадровый канал над рычагом платы.
    pub link: L,
    /// Протокол над каналом: что ответить на кадр.
    pub handler: H,
}

// {%- if graph != "true" %}
/// Узел протокола: обслуживает канал из `wiring` автоматом [`task`], не
/// выходя никогда.
pub async fn serve<L, H>(wiring: ServerWiring<L, H>)
where
    L: FrameLink,
    L::Error: FormatOrDebug,
    H: Handler,
{
    let ServerWiring {
        mut link,
        mut handler,
    } = wiring;
    task(ServerCtx {
        link: &mut link,
        handler: &mut handler,
        index: 0,
    })
    .await;
}
// {%- endif %}
