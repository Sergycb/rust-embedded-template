//! Узлы `PROTOBUF_*`: сообщения protobuf от хоста, по модулю на транспорт.
//! Канал — `link` модуля транспорта (`crate::link`) над рычагом платы в
//! памяти статиков модуля задачи.
{%- assign xs = links | split: "," %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, как в графе (`crate::graph`). {% endcomment %}
{%- assign client = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- assign lm = "crate::link::" | append: f[0] %}
{%- comment %} Без графа транспорт — UART или USB: у них модуль канала на транспорт. {% endcomment %}
{%- assign lm = lm | append: "::" | append: f[4] %}
{%- if f[3] == "protobuf" %}

/// Узел `PROTOBUF_{{ f[4] | upcase }}`.
pub mod {{ f[4] }} {
    use domain::protobuf::DeviceInfo;
    use embassy_executor::Spawner;
    use static_cell::ConstStaticCell;

    // Импорт, а не полный путь в сигнатурах: длина строки не зависит от
    // имени транспорта, и rustfmt не переносит её ни при каком имени.
    use {{ lm }}::{BUFFER, FRAME, Source, link};

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
        let mut link = link(source, LINK_BUFFER.take(), LINK_FRAME.take());
        let mut service = domain::protobuf::Service::new(
            info,
{%- if ota == "true" %}
            domain::ota::OtaClient::new(
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
