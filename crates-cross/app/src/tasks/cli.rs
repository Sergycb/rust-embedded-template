//! Узел `CONSOLE`: консоль `bareminal` поверх выбранного UART или USB CDC.
//!
//! Модуль один: консоль в проекте одна. Тип потока известен генерации, поэтому
//! задача не generic — как у остальных узлов без графа.
{%- assign xs = links | split: "," %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, как в графе (`crate::graph`). {% endcomment %}
{%- assign client = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "cli" %}
use domain::device::DeviceInfo;
use embassy_executor::Spawner;

/// Рычаг платы, поверх которого работает консоль (`board.{{ f[4] }}{% if f[0] == "usb" %}.cdc{% endif %}`).
type Source = {% if f[0] == "usb" %}bsp::usb::{{ f[4] }}::Cdc{% else %}bsp::serial::{{ f[4] }}::Serial{% endif %};

#[embassy_executor::task]
{%- if ota == "true" %}
async fn task(mut io: Source, mut info: DeviceInfo, mut ota: domain::ota::OtaClient) {
{%- else %}
async fn task(mut io: Source, mut info: DeviceInfo) {
{%- endif %}
    // Сети без графа нет: `info` печатает `mac none`, `ip none`.
    let mut login = crate::console::PASSWORD;
    domain::console::task(domain::console::ConsoleCtx {
        io: &mut io,
        info: &mut info,
        login: &mut login,
        {%- if ota == "true" %}
        ota: &mut ota,
        {%- endif %}
        index: 0,
    })
    .await;
}

/// Спавнит узел на рычаге платы `source`.
pub fn spawn(spawner: Spawner, source: Source, info: DeviceInfo) {
    {%- if ota == "true" %}
    let ota =
        domain::ota::OtaClient::new(crate::tasks::ota::CMD.requester(), domain::ota::ClientId({{ client }}));
    {%- endif %}
    let token = task(source, info{% if ota == "true" %}, ota{% endif %});
    spawner.spawn(token.expect("задача CONSOLE свежая: спавнится один раз"));
}
{%- endif %}
{%- if f[3] != "none" %}{% assign client = client | plus: 1 %}{% endif %}
{%- endfor %}
