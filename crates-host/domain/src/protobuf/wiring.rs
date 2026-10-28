//! Вход узла `PROTOBUF`: из чего собрать канал и чем отвечать.

use ports::Assemble;

/// Вход узла `PROTOBUF`: из чего собрать канал `L`, куда кодировать ответ и чем
/// отвечать.
///
/// Канал собирает узел (`L::assemble(source, buffers)` — порт
// {%- if graph == "true" %}
/// [`Assemble`]), а compose-site только раздаёт
/// (`<вид>`/`<транспорт>` — как в `crates-cross/app/src/graph.rs`):
///
/// ```ignore
/// fragments: [::domain::PROTOBUF_FRAG<crate::link::<вид>::<транспорт>::Link>
///                 as PROTOBUF_<ТРАНСПОРТ> = ::domain::protobuf::ProtobufWiring {
///                     source: board.<транспорт>,
///                     buffers: crate::link::<вид>::<транспорт>::buffers(),
///                     response: crate::link::<вид>::<транспорт>::response(),
///                     service: ::domain::protobuf::Service::new(
///                         crate::device_info(board.spec),
// {%- if ota == "true" %}
///                         ::domain::ota::OtaClient::new(ota_cmd_requester(), ::domain::ota::ClientId(0)),
// {%- endif %}
///                     ),
///                 }];
/// ```
// {%- else %}
/// [`Assemble`]), а compose-site — задача `crates-cross/app/src/tasks/protobuf.rs`
/// — только раздаёт поля платы и ссылки на свои статики.
// {%- endif %}
///
/// Написан руками, а не выведен фрагментом: слот канала инициализируется
/// сборкой из двух полей, а не чтением одного.
#[derive(Debug)]
pub struct ProtobufWiring<L: Assemble> {
    /// Рычаг платы, над которым собирается канал.
    pub source: L::Source,
    /// Память канала от приложения.
    pub buffers: L::Buffers,
    /// Буфер ответа — не короче самого длинного ответа схемы.
    pub response: &'static mut [u8],
    /// Обработчик сообщений.
    pub service: crate::protobuf::Service,
}
