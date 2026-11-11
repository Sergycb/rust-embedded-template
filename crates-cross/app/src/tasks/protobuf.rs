//! Узлы `PROTOBUF_*`: сообщения protobuf от хоста, по модулю на транспорт.
//! Канал — кадры COBS + CRC над рычагом платы, конкретным типом прямо в
//! задаче, в памяти статиков модуля задачи.
{%- assign xs = links | split: "," %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, как в графе (`crate::graph`). {% endcomment %}
{%- assign client = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "protobuf" %}

/// Узел `PROTOBUF_{{ f[4] | upcase }}`.
pub mod {{ f[4] }} {
    use domain::device::DeviceInfo;
    use embassy_executor::Spawner;
    use static_cell::ConstStaticCell;

    /// Буфер канала — закодированные сообщение и CRC (`rx_len`).
    const BUFFER: usize = adapters_logic::cobs::rx_len(domain::protobuf::REQUEST_MAX);
    /// Кадр передачи — худший случай COBS от самого длинного ответа (`tx_len`):
    /// узел отвечает из своего буфера (`send_from`), а не из буфера канала.
    const FRAME: usize = adapters_logic::cobs::tx_len(domain::protobuf::RESPONSE_MAX);

    /// Рычаг платы, над которым строится канал (`board.{{ f[4] }}{% if f[0] == "usb" %}.cdc{% endif %}`).
    type Source = {% if f[0] == "usb" %}bsp::usb::{{ f[4] }}::Cdc{% else %}bsp::serial::{{ f[4] }}::Serial{% endif %};

    // Память канала и буфер ответа: граф объявил бы её сам (`static T`), без
    // него — статики задачи. `ConstStaticCell` — нули известны при компиляции
    // и лежат в `.bss`, а не собираются на стеке при старте.
    static LINK_BUFFER: ConstStaticCell<[u8; BUFFER]> = ConstStaticCell::new([0; BUFFER]);
    static LINK_FRAME: ConstStaticCell<[u8; FRAME]> = ConstStaticCell::new([0; FRAME]);
    // Ответ узел кодирует сюда, а не в буфер канала: там может лежать
    // недочитанный кадр (`domain::protobuf`).
    static RESPONSE: ConstStaticCell<[u8; domain::protobuf::RESPONSE_MAX]> =
        ConstStaticCell::new([0; domain::protobuf::RESPONSE_MAX]);

    #[embassy_executor::task]
    async fn task(source: Source, info: DeviceInfo) {
        // Канал — над рычагом платы; статики берутся один раз: задача
        // спавнится один раз за запуск.
        let mut link =
            adapters_logic::cobs::Cobs::new(source, LINK_BUFFER.take(), LINK_FRAME.take());
        let mut service = domain::protobuf::Service::new(
            info,
{%- if ota == "true" %}
            domain::ota::Client::new(
                crate::tasks::ota::CMD.requester(),
                domain::ota::ClientId({{ client }}),
            ),
{%- endif %}
        );
        domain::protobuf::task(domain::protobuf::ProtobufCtx {
            link: &mut link,
            service: &mut service,
            response: RESPONSE.take(),
            index: 0,
        })
        .await;
    }

    /// Спавнит узел над рычагом платы `source`.
    pub fn spawn(spawner: Spawner, source: Source, info: DeviceInfo) {
        let token = task(source, info);
        spawner.spawn(token.expect("задача PROTOBUF_{{ f[4] | upcase }} свежая: спавнится один раз"));
    }
}
{%- endif %}
{%- if f[3] != "none" %}{% assign client = client | plus: 1 %}{% endif %}
{%- endfor %}
