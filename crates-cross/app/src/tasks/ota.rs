//! Узел `OTA`: подтверждает образ (`mark_booted`) в стартовом действии, а без
//! него bootloader откатит обновление на следующем сбросе; образ принимает
//! командами на вход [`CMD`].
{%- assign xs = links | split: "," %}
{%- assign askers = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "canopen" %}{% assign askers = askers | plus: 1 %}{% endif %}
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

// Мест ожидания у `CMD` — по его клиентам, а не умолчание `sync-request` (4):
// иначе ждущие сверх четырёх будят друг друга по кругу. Клиентов —
// {{ ota_cmd_clients }}: задачи протоколов, кроме CANopen (тот на `ASK`); число считает
// генерация. Подключили к `CMD` своего клиента — прибавьте его к числу здесь.
// Тем же числом назван контекст узла: параметр фрагмента `OTA_FRAG` он несёт
// и без графа.
const CMD_PARKED: usize = domain::ota::cmd_parked({{ ota_cmd_clients }});

/// Адаптер обновления над флешем платы.
type Updater = domain::ota::Updater<bsp::FirmwareFlash>;
{%- if askers > 0 %}

/// Ребро входа `ASK` — доля одного клиента в его очереди.
type OtaEdge =
    Edge<'static, CriticalSectionRawMutex, domain::ota::OtaRequest, domain::ota::OtaReply>;
{%- endif %}

/// Вход команд узла — запрос с ответом, ёмкостью `domain::ota::CMD_BYTES`. Клиента{% if has_modbus != "true" and has_protobuf != "true" and has_canopen != "true" %} получает ваш протокол
/// (`domain::ota::OtaClient` над `CMD.requester()`); без него узел только подтверждает образ{% else %} получает каждая задача протокола —
/// `domain::ota::OtaClient` с номером своего транспорта{% endif %}.
pub static CMD: RpcService<
    CriticalSectionRawMutex,
    domain::ota::OtaRequest,
    domain::ota::OtaReply,
    { domain::ota::CMD_BYTES },
    CMD_PARKED,
> = RpcService::new();

/// Вход команд без ожидания исхода.{% if askers > 0 %} Клиенты — задачи CANopen, по ребру на транспорт с квотой
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

/// Рёбра входа `ASK`: ребро `i` — `i`-го транспорта CANopen в `links`, слот `i`.
{%- assign k = 0 %}
pub static ASK_EDGES: [OtaEdge; {{ askers }}] = [
{%- for l in xs %}{% assign f = l | split: ":" %}{% if f[3] == "canopen" %}{% if k > 0 %}, {% endif %}ASK.edge({{ k }}, {{ k }}, 1){% assign k = k | plus: 1 %}{% endif %}{% endfor -%}
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

/// Спавнит узел над разделами обновления платы — адаптер обновления строит задача;
/// `image` — свойства этого образа (`crate::version`): версия для защиты от
/// отката и подтверждение.
///
/// # Panics
///
/// Если задачу уже спавнили: `spawn` зовётся один раз за запуск.
pub fn spawn(
    spawner: embassy_executor::Spawner,
    firmware: bsp::FirmwareFlash,
    image: crate::version::Image,
) {
    // Тестовый образ `app-unconfirmed` начинает «уже подтверждённым» и
    // `mark_booted` не зовёт: следующий сброс его откатывает
    // (`cargo xtask test host-target`, docs/ota.md). В задачу — только два
    // поля, а не `image` целиком: аргументы задачи и есть её слоты, и текст
    // версии узлу OTA ни к чему.
    let token = task(firmware, image.version, image.confirmed);
    spawner.spawn(token.expect("задача OTA свежая: спавнится один раз"));
}

#[embassy_executor::task]
async fn task(firmware: bsp::FirmwareFlash, mut running_version: u32, mut confirmed: bool) {
    // Адаптер обновления — над разделами обновления платы.
    let mut flash = Updater::new(firmware);
    let mut reboot = bsp::reset::Reboot;
    domain::ota::task(domain::ota::OtaCtx::<'_, _, _, CMD_PARKED> {
        flash: &mut flash,
        reboot: &mut reboot,
        running_version: &mut running_version,
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
