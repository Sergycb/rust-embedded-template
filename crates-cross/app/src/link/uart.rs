//! Транспорты протоколов поверх UART платы — подмодуль на транспорт: UART на
//! плате бывает несколько, и у каждого свои буферы.
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "uart" %}
{%- if f[3] == "modbus" or f[3] == "protobuf" %}

/// Транспорт `{{ f[4] }}`: {% if f[3] == "modbus" %}кадр RTU с CRC{% else %}кадры COBS + CRC{% endif %} поверх UART `{{ f[2] }}` платы.
pub mod {{ f[4] }} {
{%- if f[3] == "protobuf" %}
    /// Буфер канала — закодированные сообщение и CRC (`rx_len`).
    pub const BUFFER: usize = ::adapters_logic::cobs::rx_len(::domain::protobuf::REQUEST_MAX);
    /// Кадр передачи — худший случай COBS от самого длинного ответа (`tx_len`):
    /// узел отвечает из своего буфера (`send_from`), а не из буфера канала.
    pub const FRAME: usize = ::adapters_logic::cobs::tx_len(::domain::protobuf::RESPONSE_MAX);

{%- endif %}
    /// Рычаг платы, над которым строится канал (`board.{{ f[4] }}`).
    pub type Source = bsp::serial::{{ f[4] }}::Serial;

    /// Канал узла `{% if f[3] == "modbus" %}MODBUS{% else %}PROTOBUF{% endif %}_{{ f[4] | upcase }}`.
{%- if f[3] == "modbus" %}
    pub type Link = ::adapters_logic::modbus::Rtu<Source>;

    /// Канал над рычагом платы: кадр RTU памяти извне не просит.
    pub fn link(source: Source) -> Link {
        ::adapters_logic::modbus::Rtu::new(source)
    }
{%- else %}
    pub type Link = ::adapters_logic::cobs::Cobs<'static, Source>;

    /// Канал над рычагом платы в памяти от compose-site: буфер канала `buf`
    /// ([`BUFFER`]) и кадра передачи `frame` ([`FRAME`]).
    pub fn link(source: Source, buf: &'static mut [u8], frame: &'static mut [u8]) -> Link {
        ::adapters_logic::cobs::Cobs::new(source, buf, frame)
    }
{%- endif %}
}
{%- endif %}
{%- endif %}
{%- endfor %}
