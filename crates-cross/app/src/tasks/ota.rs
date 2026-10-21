//! Узел `OTA`: подтверждает образ (`mark_booted`) в стартовом действии, а без
//! него bootloader откатит обновление на следующем сбросе; образ принимает
//! командами на вход [`CMD`].
{%- assign xs = links | split: "," %}

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
{%- if has_usb == "true" %}
use embassy_sync::signal::Signal;
{%- endif %}
use sync_request::RpcService;

/// Вход команд узла — то, что с графом заводит `request:` фрагмента
/// `OTA_FRAG`, той же ёмкости (`domain::ota::CMD_BYTES`). Клиента{% if has_modbus != "true" and has_protobuf != "true" and has_canopen != "true" %} получает ваш протокол
/// (`domain::ota::OtaClient` над `CMD.requester()`); без него узел только подтверждает образ{% else %} получает каждая задача протокола —
/// `domain::ota::OtaClient` с номером своего транспорта{% endif %}.
pub static CMD: RpcService<
    CriticalSectionRawMutex,
    domain::ota::OtaRequest,
    domain::ota::OtaReply,
    { domain::ota::CMD_BYTES },
> = RpcService::new();

{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}

/// Образ подтверждён (или попытка была) — то, что с графом заводит `ready`
/// фрагмента `OTA_FRAG`. Ждёт его задача `USB_{{ f[4] | upcase }}`: перечисление, попавшее
/// на стирание `STATE` при подтверждении, хост бракует (`domain::ota`,
/// `begin`). Сигнал на задачу: у `Signal` один ждущий.
pub static READY_{{ f[4] | upcase }}: Signal<CriticalSectionRawMutex, ()> = Signal::new();
{%- endif %}
{%- endfor %}

#[embassy_executor::task]
async fn task(mut flash: crate::ota::Updater, mut mode: domain::ota::Mode) {
    let mut reboot = bsp::cpu::Reboot;
    let mut confirmed = false;
    domain::ota::task(domain::ota::OtaCtx {
        flash: &mut flash,
        reboot: &mut reboot,
        mode: &mut mode,
        confirmed: &mut confirmed,
        cmd: CMD.server().expect("сервер входа CMD один — у этой задачи"),
        // Без графа `Readiness` строит не макрос, а этот вызов{% if has_usb == "true" %} — над своими
        // сигналами, как макрос над своими.
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

/// Собирает адаптер обновления над разделами платы и спавнит узел. Адаптер
/// уезжает в задачу владением; режим —
/// {% if signed == "true" %}`Signed` с версией этого образа (`crate::image::FW_VERSION`){% else %}`Plain`{% endif %}.
pub fn spawn(spawner: embassy_executor::Spawner, partitions: bsp::ota::Partitions) {
    let flash = crate::ota::new(partitions);
{%- if signed == "true" %}
    let mode = domain::ota::Mode::Signed {
        running_version: crate::image::FW_VERSION,
    };
{%- else %}
    let mode = domain::ota::Mode::Plain;
{%- endif %}
    spawner.spawn(task(flash, mode).expect("задача OTA свежая: спавнится один раз"));
}
