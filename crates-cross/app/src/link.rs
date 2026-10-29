//! Транспорты протоколов: файл на вид транспорта (`{{ link_kinds | split: "," | join: "`, `" }}`),
//! а у видов, которых на плате бывает несколько, — подмодуль на транспорт с
//! протоколом, с именем транспорта (`crate::link::<вид>::<транспорт>`).
//!
//! Здесь — тип канала (`Link`: какое кадрирование над каким рычагом платы,
//! выбор генерации), его размеры и `link(…)`, который строит канал над рычагом
//! платы в памяти от compose-site: {% if graph == "true" %}её объявляет граф (`static [u8; N]` в привязке
//! узла){% else %}её держат статики модуля задачи (`crate::tasks`){% endif %}. Узел протокола ({% if has_modbus == "true" %}`domain::server`{% endif %}{% if has_modbus == "true" and has_protobuf == "true" %}, {% endif %}{% if has_protobuf == "true" %}`domain::protobuf`{% endif %})
//! получает готовый канал. Своё кадрирование — другой тип `Link` и его
//! `link`; свой размер буфера или порт — константы там.
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
{%- if protocol_sockets != "0" %}

/// Буфер сокета протокола — с запасом на {% if modbus_mbap == "true" %}самый длинный кадр Modbus TCP
/// (260 байт){% endif %}{% if modbus_mbap == "true" and has_protobuf == "true" %} и {% endif %}{% if has_protobuf == "true" %}сообщение protobuf (`domain::protobuf::REQUEST_MAX`, `RESPONSE_MAX`){% endif %}.
pub const SOCKET_BUFFER: usize = 1024;
{%- endif %}
