#![no_std]
#![no_main]
#![allow(
    missing_debug_implementations,
    reason = "бинарник ничего не экспортирует, а shadow-rs генерирует pub-типы без Debug"
)]

use shadow_rs::shadow;
shadow!(build);
mod cpu_load;
{%- if graph == "true" %}
mod graph;
{%- endif %}
{%- if signed == "true" %}
mod image;
{%- endif %}

use defmt::info;
// RTT остаётся дефолтным транспортом в обоих профилях — сырой шаблон не знает,
// какой конкретный USART на плате пользователь отведёт под лог без пробника.
// Паттерн замены (UART, без RTT/пробника) для release — в
// docs/modules/app-graph.md, подключается по мере готовности платы (Board
// должен будет отдавать объект транспорта, собранный из реальной периферии).
use defmt_rtt as _;

use embassy_executor::raw::Executor;
use embassy_time::Instant;
use static_cell::StaticCell;

/// Исполнитель задач — свой, а не из `#[embassy_executor::main]` (почему — в
/// `main`). `StaticCell`, а не `ConstStaticCell`: `Executor::new` не `const`.
static EXECUTOR: StaticCell<Executor> = StaticCell::new();

{%- if graph != "true" %}

// Задачи узлов проекта без графа — то, что с графом делает `fragments:` в
// `graph.rs`. `Spawner::spawn` берёт токен, а не функцию, а embassy-задача не
// может быть generic, поэтому каждая типизирована объектом платы (`bsp`).
// Контекст узла собирается здесь из локальных переменных кадра: то, что в графе
// держит `static`-слот, без графа живёт в задаче. Сама задача — `task` узла из
// `domain`, та же, что гоняет граф.
{%- if ota == "true" %}

/// Узел `OTA`: подтверждает образ (`mark_booted`) в стартовом действии, а без
/// него bootloader откатит обновление на следующем сбросе; образ принимает
/// командами на вход [`CMD`](ota::CMD).
mod ota {
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
{%- if interface == "usb" %}
    use embassy_sync::signal::Signal;
{%- endif %}
    use sync_request::RpcService;

    /// Вход команд узла — то, что с графом заводит `request:` фрагмента
    /// `OTA_FRAG`, той же ёмкости (`domain::ota::CMD_BYTES`). Клиента{% if protocol == "none" %} получает ваш протокол
    /// (`CMD.requester()`); без него узел только подтверждает образ{% else %} получает задача протокола{% endif %}.
    pub static CMD: RpcService<
        CriticalSectionRawMutex,
        domain::ota::OtaCommand,
        domain::ota::OtaReply,
        { domain::ota::CMD_BYTES },
    > = RpcService::new();

{%- if interface == "usb" %}
    /// Образ подтверждён (или попытка была) — то, что с графом заводит `ready`
    /// фрагмента `OTA_FRAG`. Ждёт его задача `USB`: перечисление, попавшее на
    /// стирание `STATE` при подтверждении, хост бракует (`domain::ota`, `begin`).
    pub static READY: Signal<CriticalSectionRawMutex, ()> = Signal::new();
{%- endif %}

    #[embassy_executor::task]
    pub async fn task(mut flash: bsp::ota::Updater, mut mode: domain::ota::Mode) {
        let mut reboot = bsp::cpu::Reboot;
        let mut confirmed = false;
        domain::ota::task(domain::ota::OtaCtx {
            flash: &mut flash,
            reboot: &mut reboot,
            mode: &mut mode,
            confirmed: &mut confirmed,
            cmd: CMD.server().expect("сервер входа CMD один — у этой задачи"),
            // Без графа `Readiness` строит не макрос, а этот вызов{% if interface == "usb" %} — над своим
            // сигналом, как макрос над своим.
            ready: supervisor::Readiness::new(|| READY.signal(())),{% else %}; готовности
            // OTA здесь никто не ждёт.
            ready: supervisor::Readiness::new(|| {}),{% endif %}
            index: 0,
        })
        .await;
    }
}
{%- endif %}
{%- if interface == "tcp" or interface == "udp" %}

/// Узел `NET`: фоновая работа стека `embassy-net`.
mod net {
    #[embassy_executor::task]
    pub async fn task(mut runner: domain::net::Runner<'static, bsp::net::Device>) {
        domain::net::task(domain::net::NetCtx {
            runner: &mut runner,
            index: 0,
        })
        .await;
    }
}
{%- endif %}
{%- if interface == "usb" %}

/// Узел `USB`: фоновая работа устройства `embassy-usb`.
mod usb {
    #[embassy_executor::task]
    pub async fn task(mut device: bsp::usb::Device) {
{%- if ota == "true" %}
        // На шину — после подтверждения образа: с графом это `deps: [OTA ready]`
        // фрагмента `USB_FRAG`.
        crate::ota::READY.wait().await;
{%- endif %}
        domain::usb::task(domain::usb::UsbCtx {
            device: &mut device,
            index: 0,
        })
        .await;
    }
}
{%- endif %}
{%- if protocol == "modbus" %}

/// Узел `MODBUS`: Modbus slave с пустой картой регистров, адрес 1.
mod modbus {
    #[embassy_executor::task]
    pub async fn task(mut link: bsp::link::Link) {
{%- if ota == "true" %}
        let mut regs = domain::modbus::OtaRegisters::new(
            domain::modbus::EmptyRegisters,
            crate::ota::CMD.requester(),
        );
{%- else %}
        let mut regs = domain::modbus::EmptyRegisters;
{%- endif %}
        let mut unit = 1;
        domain::modbus::task(domain::modbus::ModbusSlaveCtx {
            link: &mut link,
            regs: &mut regs,
            unit: &mut unit,
            index: 0,
        })
        .await;
    }
}
{%- elsif protocol == "protobuf" %}

/// Узел `HOST`: сообщения protobuf от хоста.
mod host {
    #[embassy_executor::task]
    pub async fn task(
        mut link: bsp::link::Link,
        info: domain::host::DeviceInfo,
        rx: &'static mut [u8],
        tx: &'static mut [u8],
    ) {
        let mut service = domain::host::Service::new(info{% if ota == "true" %}, crate::ota::CMD.requester(){% endif %});
        domain::host::task(domain::host::HostCtx {
            link: &mut link,
            service: &mut service,
            rx,
            tx,
            index: 0,
        })
        .await;
    }
}
{%- elsif protocol == "canopen" %}

/// Узел `CANOPEN`: узел CANopen, адрес 1.
mod canopen {
    #[embassy_executor::task]
    pub async fn task(mut bus: bsp::can::Can, mut settings: {% if config == "true" %}bsp::settings::Settings{% else %}adapters::settings::Unavailable{% endif %}) {
        let mut node_id = 1;
        let mut dictionary = domain::canopen::dictionary();
        let mut persist = {{config}};
        domain::canopen::task(domain::canopen::CanOpenCtx {
            bus: &mut bus,
            node_id: &mut node_id,
            dictionary: &mut dictionary,
            settings: &mut settings,
            persist: &mut persist,
{%- if ota == "true" %}
            ota_cmd: &mut crate::ota::CMD.requester(),
{%- endif %}
            index: 0,
        })
        .await;
    }
}
{%- endif %}
{%- endif %}
{%- if protocol == "protobuf" %}

/// Сведения об устройстве для ответа хосту на `info`: версия и коммит — из
/// `shadow-rs` (те же, что в баннере старта), остальное — с платы.
fn device_info(spec: bsp::ChipSpec) -> domain::host::DeviceInfo {
    domain::host::DeviceInfo {
        version: build::PKG_VERSION,
        commit: build::SHORT_COMMIT,
        chip: spec.chip,
        uid: spec.uid,
        flash_size: spec.flash_size,
        sys_hz: spec.sys_hz,
    }
}
{%- endif %}

#[bsp::cpu::entry]
fn main() -> ! {
    // Первая же строка лога отвечает на вопрос «а что вообще залито в плату»:
    // версия пакета и коммит, из которого собран образ (их подставляет
    // `shadow-rs` в build.rs). Без этого build-info собиралась бы впустую, а по
    // OTA легко получить плату с прошивкой, происхождение которой неизвестно.
    info!(
        "app: starting {} ({})",
        build::PKG_VERSION,
        build::SHORT_COMMIT,
    );

    // Причина прошлого падения, если оно было: паникёр release-профиля успел
    // записать её в регион PANIC, а тот пережил сброс (RAM, но вне секций,
    // которые cortex-m-rt инициализирует при старте). Читается один раз —
    // `panic-persist` стирает свою магию, — поэтому печатать надо сразу и до
    // всего остального: следующий сброс эту строку уже не покажет.
    //
    // ДО `Board::new`, и это важно: тот умеет паниковать сам (негодная
    // конфигурация тактирования, негодный раздел `CONFIG`), а в release
    // паника это сброс. Печатай мы после него — устройство, падающее внутри
    // `new`, крутилось бы в цикле перезагрузок молча.
    //
    // Только release: в dev паникёр — `panic-probe`, дамп никто не пишет, и
    // читать было бы нечего (см. `bsp::panic`).
    #[cfg(not(debug_assertions))]
    if let Some(reason) = bsp::panic::previous() {
        defmt::error!("app: предыдущий запуск упал: {}", reason);
    }

    let board = bsp::Board::new();
    // Следом — на чём именно: чип и его заводской номер, по которому
    // экземпляр платы отличается от соседнего.
    info!("app: {}", board.spec);

    // Исполнитель — свой, а не `#[embassy_executor::main]`, и это единственная
    // причина, по которой `main` здесь синхронный. У макроса `WFE` живёт
    // внутри `Executor::run`, между двумя вызовами `poll`, и снаружи их не
    // видно — а именно там проходит всё время, когда задач нет. Измерению
    // загрузки нужно знать, сколько тиков микроконтроллер стоял, и кроме
    // явного `WFE` этой величины взять неоткуда. Подробности — в модуле
    // `cpu_load` (docs/modules/app-cpu-load.md).
    //
    // `usize::MAX` вместо нуля — тот же «контекст», что и у макроса: на
    // thread-режиме `__pender` делает `SEV` (пока `executor-interrupt` не
    // включён, ровно так же), а не `WFE`, то есть пробуждение задач не теряет.
    // Цикл ниже ставит `WFE` сам, и всё, что остаётся на долю pender'а, — это
    // возврат из него, а не сон.
    let executor = EXECUTOR.init(Executor::new(usize::MAX as *mut ()));
    let spawner = executor.spawner();
{%- if graph == "true" %}

    // Граф задач — единственное место, где в этом проекте появляются задачи.
    // Голый `spawner.spawn(..)` рядом с ним завёл бы вторую, неуправляемую
    // ветку: без `restart:`, `deps:` и `watchdog:` (см. graph.rs).
    //
    // `board` уезжает сюда целиком — это объявленный графом `boot:`-объект,
    // из которого его инициализаторы забирают объекты частичными move.
    // Такие заборы уже есть — сторож в блоке `watchdog:`{% if ota == "true" %}, вход узла
    // OTA в `fragments:` (`board.ota`){% endif %} — и здесь же
    // окажутся ваши: `resources: [SLOT: T = board.<поле>]` вместо
    // `provide_<slot>(..)` перед этой строкой. Оттого и последняя строка
    // `spawn_all`: после неё `board` принадлежит прологу.
    //
    // Аппаратный сторож запускается ВНУТРИ этого вызова — инициализатором
    // блока `watchdog:`, — а не в `Board::new` и не строкой выше: с этого
    // момента отсчёт идёт, а кормить начнёт только тикер графа, и чем короче
    // промежуток между этими двумя моментами, тем меньше шансов уйти в
    // перезагрузки молча. В прологе там остаётся только спавн узлов; всё своё
    // долгое (проверка подписи, запись во flash) держите ВЫШЕ этой строки.{% if ota == "true" %}
    //
    // Но в release сторож к этой строке уже тикает: его запустил bootloader
    // (на той же цифре `bsp::wdg::HW_TIMEOUT_US`, 10 с), и весь
    // путь от прыжка сюда — `Board::new` и ваше долгое —
    // обязан в него уложиться (docs/watchdog.md).{% endif %}
    //
    // Паника, а не `let _ =`, и это не педантизм: сторож запускается в
    // прологе, ДО спавна узлов, поэтому отказ (узел уже занят) оставляет
    // железо тикающим, а кормить его некому — проглоти вы `Err`, плата ушла
    // бы в сброс через `bsp::wdg::HW_TIMEOUT_US` без единой строки о причине.
    // Через `Display2Format`, а не `expect`: `Debug` у ошибки графа нет —
    // `supervisor` убрал его ради флеша, имя узла и вид отказа даёт `Display`.
    if let Err(err) = graph::spawn_all(&spawner, board) {
        defmt::panic!("app: spawn_all: {}", defmt::Display2Format(&err));
    }
{%- else %}

    // Графа нет, поэтому задачу, которая меряет загрузку, спавнит сам `main`.
    // `Spawner::spawn` берёт `SpawnToken`, а не функцию, поэтому обёртка
    // `#[embassy_executor::task]` объявлена в `cpu_load.rs`: в домене
    // executor-обвязки быть не может, а здесь ровно место для неё. Токен
    // выдаёт она же, и `expect` — на нём: `Spawner::spawn` отказ не вернёт,
    // а проглоченный `SpawnError::Busy` означал бы, что загрузку не меряет
    // никто, и это молча.
    let token = cpu_load::task().expect("задача CPU_LOAD свежая: спавнится один раз");
    spawner.spawn(token);
{%- if ota == "true" %}

    // Узел OTA без графа спавнится так же: он подтверждает образ
    // (`mark_booted`) в стартовом действии, а без него bootloader откатит
    // обновление на следующем сбросе. Адаптер обновления уезжает в задачу
    // владением{% if signed == "true" %}; режим — `Signed` с версией этого образа
    // (`image::FW_VERSION`){% else %}; режим — `Plain`{% endif %}; команды — через `ota::CMD`.
{%- if signed == "true" %}
    let mode = domain::ota::Mode::Signed {
        running_version: image::FW_VERSION,
    };
{%- else %}
    let mode = domain::ota::Mode::Plain;
{%- endif %}
    let token = ota::task(board.ota, mode).expect("задача OTA свежая: спавнится один раз");
    spawner.spawn(token);
{%- endif %}
{%- if interface == "tcp" or interface == "udp" or interface == "usb" or protocol != "none" %}

    // Узлы транспорта и протокола без графа спавнятся так же. Без графа нет ни
    // сторожа, ни `restart:`: узлы не падают, а зависший остаётся зависшим —
    // сторожить исполнитель вам.
{%- endif %}
{%- if interface == "tcp" or interface == "udp" %}
    let token = net::task(board.net.runner).expect("задача NET свежая: спавнится один раз");
    spawner.spawn(token);
{%- endif %}
{%- if interface == "usb" %}
    let token = usb::task(board.{% if protocol_link == "true" %}usb_device{% else %}usb.device{% endif %}).expect("задача USB свежая: спавнится один раз");
    spawner.spawn(token);
{%- endif %}
{%- if protocol == "modbus" %}
    let token = modbus::task(board.link).expect("задача MODBUS свежая: спавнится один раз");
    spawner.spawn(token);
{%- elsif protocol == "protobuf" %}
    let token = host::task(
        board.link,
        device_info(board.spec),
        board.host_buffers.rx,
        board.host_buffers.tx,
    )
    .expect("задача HOST свежая: спавнится один раз");
    spawner.spawn(token);
{%- elsif protocol == "canopen" %}
    let token = canopen::task(board.can, {% if config == "true" %}board.settings{% else %}::adapters::settings::Unavailable{% endif %})
        .expect("задача CANOPEN свежая: спавнится один раз");
    spawner.spawn(token);
{%- endif %}
{%- endif %}

    // Цикл исполнителя: спим, пока нечего делать, и отмечаем в счётчике, сколько
    // тиков проспали. `wfe()` возвращается и по событию (SEV от `spawn`, от
    // пробуждения задачи), и по таймеру — от того, что сработал следующий
    // `Timer` задачи, — так что это единственное место, где цикл ждёт.
    // `poll` — `unsafe`, и это то же обещание, что у `Executor::run`: поллить
    // исполнитель можно только из единственного места, а оно здесь.
    loop {
        let before = Instant::now().as_ticks();
        bsp::cpu::wait_for_event();
        cpu_load::record_sleep_ticks(Instant::now().as_ticks().saturating_sub(before));
        // SAFETY: поллить исполнитель можно только из единственного места, а
        // оно здесь — цикл ниже единственный, кто его крутит, и `spawn_all` к
        // этому моменту уже отработал. Ровно то же обещание, что у `Executor::run`.
        //
        // `#[expect(unsafe_code)]` — вместе с остальными в `crates-cross` (см. `crates-cross/Cargo.toml`)
        // (`boot/src/main.rs`, `app/src/image.rs`): unsafe в прошивке допустим, но каждое
        // место обязано быть перечислено и объяснено, а линт `unsafe_code`
        // заставляет перечислять.
        #[expect(unsafe_code, reason = "обоснование — в комментарии выше")]
        unsafe {
            executor.poll();
        }
    }
}
