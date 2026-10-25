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
    use static_cell::ConstStaticCell;

    // Буфер канала — сообщение и CRC (`rx_len`); кадру передачи — место под
    // худший случай COBS от всего буфера канала (`tx_len`).
    const BUFFER: usize = ::adapters::cobs::rx_len(crate::link::PROTOBUF_MESSAGE);
{% endif %}
    /// Канал узла `{% if f[3] == "modbus" %}MODBUS{% else %}PROTOBUF{% endif %}_{{ f[4] | upcase }}`; собирается над UART платы (`board.{{ f[4] }}`).
{%- if f[3] == "modbus" %}
    pub type Link = ::adapters::modbus::Rtu<bsp::serial::{{ f[4] }}::Serial>;
{%- else %}
    pub type Link = ::adapters::cobs::Cobs<'static, bsp::serial::{{ f[4] }}::Serial>;
{%- endif %}
{%- if graph != "true" %}

    /// Рычаг платы, над которым собирается канал, — тип по имени для задачи
    /// (задачи embassy не generic).
    pub type Source = bsp::serial::{{ f[4] }}::Serial;
{%- endif %}
{%- if f[3] == "protobuf" %}

    // `ConstStaticCell`, а не `StaticCell`: значения известны при компиляции и
    // лежат в `.bss`, а не собираются на стеке при старте.
    static PROTOBUF_BUFFER: ConstStaticCell<[u8; BUFFER]> = ConstStaticCell::new([0; BUFFER]);
    static PROTOBUF_FRAME: ConstStaticCell<[u8; ::adapters::cobs::tx_len(BUFFER)]> =
        ConstStaticCell::new([0; ::adapters::cobs::tx_len(BUFFER)]);
    // Ответ узел кодирует сюда, а не в буфер канала: там может лежать
    // недочитанный кадр (`domain::protobuf`).
    static PROTOBUF_RESPONSE: ConstStaticCell<[u8; crate::link::PROTOBUF_MESSAGE]> =
        ConstStaticCell::new([0; crate::link::PROTOBUF_MESSAGE]);
{%- endif %}

    /// Память канала — ссылки на статики выше. Статики берутся ровно один раз:
    /// узел собирается один раз за запуск.
{%- if f[3] == "modbus" %}
    pub fn buffers() {}
{%- else %}
    pub fn buffers() -> ::adapters::cobs::CobsBuffers {
        ::adapters::cobs::CobsBuffers {
            buf: PROTOBUF_BUFFER.take(),
            frame: PROTOBUF_FRAME.take(),
        }
    }

    /// Буфер ответа узла — ссылка на статик выше; берётся ровно один раз.
    pub fn response() -> &'static mut [u8] {
        PROTOBUF_RESPONSE.take()
    }
{%- endif %}
}
{%- endif %}
{%- endif %}
{%- endfor %}
