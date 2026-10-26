//! Вход узла протокола: из чего собрать канал и чем отвечать.
// {%- if graph != "true" %}
//!
//! Без графа здесь же и задача, которая собирает узел из входа, — `serve`.
// {%- endif %}

// {%- if graph != "true" %}
use defmt_or_log::FormatOrDebug;
// {%- endif %}
use ports::Assemble;
// {%- if graph != "true" %}
use ports::{FrameLink, Handler};
use supervisor::runtime::TaskExit;

use super::{ServerCtx, task};
// {%- endif %}

/// Вход узла: из чего собрать канал `L` и чем отвечать.
///
/// Канал собирает узел (`L::assemble(source, buffers)` — порт
/// [`Assemble`]), а compose-site только раздаёт: рычаг платы, ссылки на
// {%- if graph == "true" %}
/// статики приложения и обработчик. Какой канал под каким рычагом — тип `L`,
/// его выбирает генерация параметром фрагмента (`<вид>`/`<транспорт>` — как в
/// `crates-cross/app/src/graph.rs`):
///
/// ```ignore
/// fragments: [::domain::SERVER_FRAG<crate::link::<вид>::<транспорт>::Link, ::domain::modbus::Slave<crate::modbus::Handle>>
///                 as MODBUS_<ТРАНСПОРТ> = ::domain::server::ServerWiring {
///                     source: board.<транспорт>,
///                     buffers: crate::link::<вид>::<транспорт>::buffers(),
///                     handler: ::domain::modbus::Slave::new(1, crate::modbus::handle()),
///                 }];
/// ```
// {%- else %}
/// статики приложения и обработчик. Какой канал под каким рычагом — тип `L`,
/// его выбирает генерация в задаче `crates-cross/app/src/tasks/modbus.rs`.
// {%- endif %}
///
/// Буфер кадра — у канала (`FrameLink::buffer`), а не у узла: его размер
/// задаёт протокол над каналом, и канал собирается вместе с ним.
///
/// Написана руками, а не выведена фрагментом (`boot: … struct`): слот
/// канала инициализируется сборкой из двух полей, а не чтением одного.
#[derive(Debug)]
pub struct ServerWiring<L: Assemble, H> {
    /// Рычаг платы, над которым собирается канал.
    pub source: L::Source,
    /// Память канала от приложения.
    pub buffers: L::Buffers,
    /// Протокол над каналом: что ответить на кадр.
    pub handler: H,
}

// {%- if graph != "true" %}
/// Узел протокола: собирает канал из `wiring` и обслуживает его автоматом
/// [`task`], не выходя никогда.
pub async fn serve<L, H>(wiring: ServerWiring<L, H>) -> TaskExit
where
    L: Assemble + FrameLink,
    L::Error: FormatOrDebug,
    H: Handler,
{
    let mut link = L::assemble(wiring.source, wiring.buffers);
    let mut handler = wiring.handler;
    task(ServerCtx {
        link: &mut link,
        handler: &mut handler,
        index: 0,
    })
    .await
}
// {%- endif %}
