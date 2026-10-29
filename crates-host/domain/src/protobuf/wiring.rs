//! Вход узла `PROTOBUF`: канал, буфер ответа и чем отвечать.

/// Вход узла `PROTOBUF`: канал `L`, куда кодировать ответ и чем отвечать.
///
// {%- if graph == "true" %}
/// Канал и память строит compose-site (`<вид>`/`<транспорт>` — как в
/// `crates-cross/app/src/graph.rs`):
///
/// ```ignore
/// fragments: [::domain::PROTOBUF_FRAG<crate::link::<вид>::<транспорт>::Link>
///                 as PROTOBUF_<ТРАНСПОРТ> = ::domain::protobuf::ProtobufWiring {
///                     link: crate::link::<вид>::<транспорт>::link(board.<транспорт>, static [u8; N], static [u8; M]),
///                     response: static [u8; ::domain::protobuf::RESPONSE_MAX],
///                     service: ::domain::protobuf::Service::new(
///                         crate::device_info(board.spec),
// {%- if ota == "true" %}
///                         ::domain::ota::OtaClient::new(ota_cmd_requester(), ::domain::ota::ClientId(0)),
// {%- endif %}
///                     ),
///                 }];
/// ```
// {%- else %}
/// Канал и память строит задача `crates-cross/app/src/tasks/protobuf.rs`.
// {%- endif %}
#[derive(Debug)]
pub struct ProtobufWiring<L> {
    /// Кадровый канал над рычагом платы.
    pub link: L,
    /// Буфер ответа — не короче самого длинного ответа схемы.
    pub response: &'static mut [u8],
    /// Обработчик сообщений.
    pub service: crate::protobuf::Service,
}
