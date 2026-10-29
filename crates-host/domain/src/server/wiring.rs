//! Задача узла без графа — `serve` над входом фрагмента (`ServerWiring`).

// {%- if graph != "true" %}
use defmt_or_log::FormatOrDebug;
use ports::FrameLink;

use super::{Handler, ServerCtx, ServerWiring, task};
// {%- endif %}

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
