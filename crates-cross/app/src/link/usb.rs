//! Транспорты протоколов поверх USB CDC платы — подмодуль на транспорт: USB на
//! плате бывает два (FS и HS), и у каждого свои буферы.
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
{%- if f[3] == "modbus" or f[3] == "protobuf" %}

/// Транспорт `{{ f[4] }}`: {% if f[3] == "modbus" %}кадр RTU с CRC{% else %}кадры COBS + CRC{% endif %} поверх USB CDC `{{ f[2] }}` платы.
{%- if f[4] == "usb" %}
#[expect(
    clippy::module_inception,
    reason = "имя транспорта — имя блока `USB`, а модуль вида — `usb`"
)]
{%- endif %}
pub mod {{ f[4] }} {
{%- if f[3] == "protobuf" %}
    /// Буфер канала — сообщение и CRC (`rx_len`).
    pub const BUFFER: usize = ::adapters::cobs::rx_len(::domain::protobuf::REQUEST_MAX);
    /// Кадр передачи — худший случай COBS от всего буфера канала (`tx_len`).
    pub const FRAME: usize = ::adapters::cobs::tx_len(BUFFER);

{%- endif %}
    /// Рычаг платы, над которым строится канал (`board.{{ f[4] }}.cdc`).
    pub type Source = bsp::usb::{{ f[4] }}::Cdc;

    /// Канал узла `{% if f[3] == "modbus" %}MODBUS{% else %}PROTOBUF{% endif %}_{{ f[4] | upcase }}`.
{%- if f[3] == "modbus" %}
    pub type Link = ::adapters::modbus::Rtu<Source>;

    /// Канал над рычагом платы: кадр RTU памяти извне не просит.
    pub fn link(source: Source) -> Link {
        ::adapters::modbus::Rtu::new(source)
    }
{%- else %}
    pub type Link = ::adapters::cobs::Cobs<'static, Source>;

    /// Канал над рычагом платы в памяти от compose-site: буфер канала `buf`
    /// ([`BUFFER`]) и кадра передачи `frame` ([`FRAME`]).
    pub fn link(source: Source, buf: &'static mut [u8], frame: &'static mut [u8]) -> Link {
        ::adapters::cobs::Cobs::new(source, buf, frame)
    }
{%- endif %}
}
{%- endif %}
{%- endif %}
{%- endfor %}
