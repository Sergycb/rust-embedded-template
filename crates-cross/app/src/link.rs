//! Транспорты протоколов: файл на вид транспорта (`{{ link_kinds | split: "," | join: "`, `" }}`),
//! а у видов, которых на плате бывает несколько, — подмодуль на транспорт с
//! протоколом, с именем транспорта, и `Source` — рычаг платы под ним
//! (`crate::link::<вид>::<транспорт>`: uart, usb). У tcp/udp транспорт на плате
//! один, и модуль транспорта — сам файл вида (`crate::link::<вид>`, `link(stack, …)`).
//!
//! Здесь — тип канала (`Link`), его размеры и `link(…)`, который строит канал
//! над рычагом платы в памяти от compose-site; узел протокола получает готовый
//! канал (docs/modules/app.md, «Канал протокола: `crate::link`»).
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
