//! Транспорты протоколов: файл на вид транспорта (`{{ link_kinds | split: "," | join: "`, `" }}`),
//! а у видов, которых на плате бывает несколько, — подмодуль на транспорт с
//! протоколом, с именем транспорта (`crate::link::<вид>::<транспорт>`).
//!
//! Здесь — тип канала (`Link`: какое кадрирование над каким рычагом платы,
//! выбор генерации) и его память — статики приложения, которые `buffers()`
//! отдаёт ссылками{% if has_protobuf == "true" %}, а у узлов `PROTOBUF_*` ещё и буфер ответа (`response()`){% endif %}. Собирает канал сам узел протокола
//! ({% if has_modbus == "true" %}`domain::server`{% endif %}{% if has_modbus == "true" and has_protobuf == "true" %}, {% endif %}{% if has_protobuf == "true" %}`domain::protobuf`{% endif %}; порт `ports::Assemble`) из рычага платы и этих буферов. Своё кадрирование —
//! другой тип `Link`; свой размер буфера или порт — правьте статики там.
{% if link_kinds contains "tcp" %}
pub mod tcp;
{%- endif %}
{%- if link_kinds contains "uart" %}
pub mod uart;
{%- endif %}
{%- if link_kinds contains "udp" %}
pub mod udp;
{%- endif %}
{%- if link_kinds contains "usb" %}
pub mod usb;
{%- endif %}
{%- if has_protobuf == "true" %}

/// Самое длинное сообщение схемы, под которое заведены буферы каналов узлов
/// `PROTOBUF_*`. Схема с сообщениями длиннее — меняйте здесь; узел предупредит
/// при старте, если самый длинный ответ схемы не помещается.
const PROTOBUF_MESSAGE: usize = 256;
{%- endif %}
{%- if protocol_sockets != "0" %}

/// Буфер сокета протокола — с запасом на {% if modbus_mbap == "true" %}самый длинный кадр Modbus TCP
/// (260 байт){% endif %}{% if modbus_mbap == "true" and has_protobuf == "true" %} и {% endif %}{% if has_protobuf == "true" %}сообщение protobuf (`PROTOBUF_MESSAGE`){% endif %}.
const SOCKET_BUFFER: usize = 1024;
{%- endif %}
