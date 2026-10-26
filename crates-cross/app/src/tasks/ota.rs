//! Узел `OTA`: подтверждает образ (`mark_booted`) в стартовом действии, а без
//! него bootloader откатит обновление на следующем сбросе; образ принимает
//! командами на вход [`CMD`].
{%- assign xs = links | split: "," %}
{%- assign askers = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "canopen" or f[3] == "protobuf" %}{% assign askers = askers | plus: 1 %}{% endif %}
{%- endfor %}

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
{%- if has_usb == "true" %}
use embassy_sync::signal::Signal;
{%- endif %}
use sync_request::RpcService;
{%- if askers > 0 %}
use sync_request::ask::{AskService, Edge};
{%- else %}
use sync_request::ask::AskService;
{%- endif %}

/// Адаптер обновления над флешем платы.
type Updater = adapters::ota::Updater<bsp::FirmwareFlash, {{signed}}>;
{%- if askers > 0 %}

/// Ребро входа `ASK` — доля одного клиента в его очереди.
type OtaEdge =
    Edge<'static, CriticalSectionRawMutex, domain::ota::OtaRequest, domain::ota::OtaReply>;
{%- endif %}

/// Вход команд узла — запрос с ответом, ёмкостью `domain::ota::CMD_BYTES`. Клиента{% if has_modbus != "true" and has_protobuf != "true" and has_canopen != "true" %} получает ваш протокол
/// (`domain::ota::OtaClient` над `CMD.requester()`); без него узел только подтверждает образ{% else %} получает каждая задача протокола —
/// `domain::ota::OtaClient` с номером своего транспорта{% endif %}.{% if net_services contains "rest" %} Пул `HTTP`
/// (REST, `crate::tasks::http`) просит перезагрузку своим клиентом — номер следом за транспортами.{% endif %}
pub static CMD: RpcService<
    CriticalSectionRawMutex,
    domain::ota::OtaRequest,
    domain::ota::OtaReply,
    { domain::ota::CMD_BYTES },
> = RpcService::new();

/// Вход команд без ожидания исхода.{% if askers > 0 %} Клиенты — {% if has_canopen == "true" %}задачи CANopen{% endif %}{% if has_canopen == "true" and has_protobuf == "true" %} и {% endif %}{% if has_protobuf == "true" %}`PROTOBUF`{% endif %}, по ребру на транспорт с квотой
/// в одну команду: очередь (`Q`) — сумма квот, `E` — число рёбер.{% else %} Клиентов у него нет: очередь и
/// рёбра нулевые.{% endif %}
pub static ASK: AskService<
    CriticalSectionRawMutex,
    domain::ota::OtaRequest,
    domain::ota::OtaReply,
    {{ askers }},
    {{ askers }},
    { domain::ota::CMD_BYTES },
> = AskService::new();

{%- if askers > 0 %}

/// Рёбра входа `ASK`: ребро `i` — `i`-го транспорта {% if has_canopen == "true" and has_protobuf == "true" %}CANopen или protobuf{% elsif has_canopen == "true" %}CANopen{% else %}protobuf{% endif %} в `links`, слот `i`.
{%- assign k = 0 %}
pub static ASK_EDGES: [OtaEdge; {{ askers }}] = [
{%- for l in xs %}{% assign f = l | split: ":" %}{% if f[3] == "canopen" or f[3] == "protobuf" %}{% if k > 0 %}, {% endif %}ASK.edge({{ k }}, {{ k }}, 1){% assign k = k | plus: 1 %}{% endif %}{% endfor -%}
];
{%- endif %}

{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}

/// Образ подтверждён (или попытка была). Ждёт его задача `USB_{{ f[4] | upcase }}`: перечисление, попавшее
/// на стирание `STATE` при подтверждении, хост бракует (`domain::ota`,
/// `begin`). Сигнал на задачу: у `Signal` один ждущий.
pub static READY_{{ f[4] | upcase }}: Signal<CriticalSectionRawMutex, ()> = Signal::new();
{%- endif %}
{%- endfor %}

/// Спавнит узел над флешем платы под обновлением — адаптер обновления собирается в задаче;
/// режим —
/// {% if signed == "true" %}`Signed` с версией этого образа (`crate::version::FW_VERSION`){% else %}`Plain`{% endif %}.
pub fn spawn(spawner: embassy_executor::Spawner, firmware: bsp::FirmwareFlash) {
{%- if signed == "true" %}
    let mode = domain::ota::Mode::Signed {
        running_version: crate::version::FW_VERSION,
    };
{%- else %}
    let mode = domain::ota::Mode::Plain;
{%- endif %}
    spawner.spawn(task(firmware, mode).expect("задача OTA свежая: спавнится один раз"));
}

#[embassy_executor::task]
async fn task(firmware: bsp::FirmwareFlash, mut mode: domain::ota::Mode) {
    // Адаптер обновления — над флешем платы: `assemble` и режет его на разделы.
    let mut flash = <Updater as domain::Assemble>::assemble(firmware, ());
    let mut reboot = bsp::reset::Reboot;
    let mut confirmed = false;
    domain::ota::task(domain::ota::OtaCtx {
        flash: &mut flash,
        reboot: &mut reboot,
        mode: &mut mode,
        confirmed: &mut confirmed,
        cmd: CMD.server().expect("сервер входа CMD один — у этой задачи"),
        ask: ASK.server().expect("сервер входа ASK один — у этой задачи"),
        // `Readiness` строит этот вызов{% if has_usb == "true" %} — над своими сигналами.
        ready: supervisor::Readiness::new(|| {
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
            READY_{{ f[4] | upcase }}.signal(());
{%- endif %}
{%- endfor %}
        }),{% else %}; готовности
        // OTA здесь никто не ждёт.
        ready: supervisor::Readiness::new(|| {}),{% endif %}
        index: 0,
    })
    .await;
}
