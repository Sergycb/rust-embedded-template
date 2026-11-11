//! Узел `OTA`: принимает образ командами от узла протокола и применяет его.
//!
//! Здесь, а не в `crates-cross/app`, по общему правилу проекта
// {%- if graph == "true" %}
//! (см. `domain::heartbeat`)
// {%- endif %}
//! — что узел делает — логика, и проверяется она на хосте: адаптер
//! обновления — настоящий, над разделами в памяти.
//!
//! Образ приходит не по своему каналу: узел протокола переводит свои сообщения в
//! [`Command`](crate::ota::commands::Command) и отдаёт их узлу через один из двух
//! входов: `CMD` (`request:` фрагмента — клиент ждёт исхода в вызове) или `ASK`
//! (`serves:` — клиент отправляет команду, не дожидаясь, и забирает исход из
//! `reply()`, обслуживая тем временем свой канал). Для узла входы одинаковы:
//! команды обоих идут в один цикл. На каждую команду узел отвечает
//! [`Reply`](crate::ota::commands::Reply) — её исходом, и протокол передаёт его хосту
//! как есть: ни опрашивать статус, ни угадывать, дошёл ли кусок, хосту не
//! нужно. Узел про протокол не знает, протокол — про флеш.
//!
//! Задача — цикл с typestate-состояниями на контексте `OtaCtx`, который
//! эмитит фрагмент `OTA_FRAG`: каждое состояние — тип (`Idle`, `Receiving`,
//! `Ready`), переход из него — только через его методы, а разводка
//! (состояние × команда) — в `State::on_command`:
//!
//! * `Idle` — приёма нет; `Start` проверяет занятость и подпись и, если всё
//!   в порядке, стирает раздел под образ;
//! * `Receiving` — идёт приём от одного клиента
//!   ([`ClientId`](crate::ota::ClientId), транспорт): куски пишутся во флеш
//!   шагами [`Transfer`](crate::ota::download::Transfer), `Finish` применяет образ;
//!   `Start`/`Chunk`/`Finish` другого клиента получают `Busy`; владелец молчит
//!   дольше [`SESSION_TIMEOUT`](crate::ota::SESSION_TIMEOUT) — приём брошен;
//! * `Ready` — образ применён, обмен разделов ждёт сброса; новый `Start`
//!   отвергается как `Busy`, повтор `Finish` владельца (ответ на первый
//!   потерялся) снова получает `Ready`, ничего не записав.
//!
//! `Reboot` принимается в любом состоянии: узел отвечает, даёт протоколу
//! [`REBOOT_GRACE`](crate::ota::REBOOT_GRACE) отправить ответ и сбрасывает МК портом
//! [`Reboot`](ports::Reboot). Сброс — команда хоста, а не следствие `Finish`:
//! так хост узнаёт исход образа до сброса, а не по факту перезагрузки.
//!
//! Образ всегда подписан: `Start` без подписи отвергается до стирания, а
//! `Finish` применяет образ [`apply`](crate::ota::update::apply)
//! — с проверкой версии против версии этой прошивки, которую узел получает во
//! входе
// {%- if graph == "true" %}
//! (`OtaWiring::running_version`, `crates-cross/app/src/graph.rs`).
// {%- else %}
//! (поле `running_version` контекста `OtaCtx`, `crates-cross/app/src/tasks/ota.rs`).
// {%- endif %}
//! Подробности — `docs/ota.md`.

/// Команды узлу и исходы на них: куски образа, подпись, коды для хоста — см.
/// модуль.
pub mod commands;

/// Приём образа по кускам: сверка длины, стирание один раз, буферизация до
/// слова флеша — шаги над адаптером [`Updater`], см. модуль.
pub mod download;

/// Решение bootloader'а при старте и проверка подписи нового образа — над
/// блокирующими разделами: у bootloader'а нет исполнителя. Функции над
/// `embassy-boot`, не над портами — см. модуль.
pub mod boot;

/// Применение подписанного обновления: порядок проверок, в котором каждый
/// отказ различим, и запись подписи для bootloader'а. Функция над адаптером
/// [`Updater`] — см. модуль.
pub mod update;

/// Версия прошивки в 32 битах и правило приёма обновления (защита от
/// отката). Образец границы: `bsp` достаёт четыре байта из флеша, `domain`
/// решает, что с ними делать, — см. модуль.
pub mod version;

mod logic;
/// Адаптер обновления над `embassy-boot`: разделы `DFU` и `STATE` платы.
mod storage;

// Типы входа `CMD` — в корне `domain::ota`: их полным путём называют фрагмент
// и compose-site (`app`).
pub use commands::{Command, Reply};

// Подмодули — по теме; их типы видны как `domain::ota::…`.
pub use logic::{Idle, Ready, Receiving, State};
pub use storage::{Updater, UpdaterError};

use embassy_futures::select::{Either, select};
use embassy_time::Duration;
use embedded_storage_async::nor_flash::NorFlash;
use ports::{Partitions, Reboot};
use supervisor::{Asked, Requester, Served};

use crate::ota::commands::Rejection;
use crate::ota::download::DownloadError;
use crate::ota::logic::Ota;
use crate::ota::update::UpdateError;
use crate::tick::deadline;
use defmt_or_log::warn;

/// Ёмкость входов `CMD` и `ASK`, байт: запрос (кусок
/// [`CHUNK_BYTES`](crate::ota::commands::CHUNK_BYTES) со смещением и длиной,
/// тег варианта, номер клиента) и ответ. Одно число на
// {%- if graph == "true" %}
/// граф (`request:` и `serves:` в `OTA_FRAG`). Во фрагменте оно записано
/// литералом — макрос не принимает путь, — так что меняется в трёх местах.
// {%- else %}
/// фрагмент (`request:` и `serves:` в `OTA_FRAG`) и на `static` входов
/// (`crates-cross/app/src/tasks/ota.rs`). Во фрагменте оно записано
/// литералом — макрос не принимает путь, — так что меняется во всех этих
/// местах вместе.
// {%- endif %}
pub const CMD_BYTES: usize = 144;

const _: () = assert!(
    size_of::<Request>() <= CMD_BYTES && size_of::<Reply>() <= CMD_BYTES,
    "команда OTA не помещается во вход CMD: поднимите CMD_BYTES"
);

/// Сколько приём ждёт следующую команду, прежде чем его бросить.
///
/// Хост пропал посреди образа — раздел остаётся стёртым наполовину, но это
/// раздел обновления, не активный: ничего не сломано, следующий `Start`
/// начнёт заново. Без тайм-аута приём навсегда остался бы за замолчавшим
/// клиентом: команды других транспортов получали бы `Busy` до перезагрузки.
pub const SESSION_TIMEOUT: Duration = Duration::from_secs(30);

/// Сколько узел ждёт после ответа на `Reboot`, прежде чем сбросить МК.
///
/// Ответ уходит хосту через узел протокола, а тот работает, только когда узел
/// OTA уступит исполнитель: без паузы МК сбросился бы раньше, чем ответ
/// покинул устройство.
pub const REBOOT_GRACE: Duration = Duration::from_millis(100);

/// Номер клиента входа `CMD` — транспорта, с которого пришла команда.
///
/// Транспорт, а не удалённый хост: два хоста на одной шине для узла — один
/// клиент. Номера раздаёт compose-site (`app`), по одному на узел протокола.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct ClientId(pub u8);

/// Запрос на вход `CMD`: команда и клиент, от которого она пришла.
///
/// Клиент нужен автомату, чтобы приём образа принадлежал одному транспорту:
/// пока идёт приём, `Start`/`Chunk`/`Finish` другого клиента получают
/// `Busy`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct Request {
    /// Кто прислал команду.
    pub client: ClientId,
    /// Сама команда.
    pub command: Command,
}

/// Клиент узла OTA для узла протокола: requester входа `CMD` и свой номер.
///
/// Узел протокола зовёт [`request`](Self::request) с голой командой; номер
/// клиента подставляется сам.
#[derive(Debug, Clone, Copy)]
pub struct Client {
    requester: Requester<'static, Request, Reply>,
    id: ClientId,
}

impl Client {
    /// Клиент `id` над `requester` входа `CMD`.
    #[must_use]
    pub const fn new(requester: Requester<'static, Request, Reply>, id: ClientId) -> Self {
        Self { requester, id }
    }

    /// Команда узлу OTA от этого клиента и её исход.
    ///
    /// # Errors
    ///
    /// [`NodeLost`] — узел взял запрос, но не ответил.
    pub async fn request(&self, command: Command) -> Result<Reply, NodeLost> {
        self.requester
            .request(Request {
                client: self.id,
                command,
            })
            .await
            // `RequestDroppedError` — единичный тип: причины, которую стоило бы
            // хранить, в нём нет.
            .map_err(|_dropped| NodeLost)
    }
}

/// Узел OTA взял команду, но ответа на неё не будет: задача узла
/// прервалась посреди обработки.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
pub struct NodeLost;

impl core::fmt::Display for NodeLost {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("узел OTA не ответил на команду")
    }
}

impl core::error::Error for NodeLost {}

/// Обязанность ответить на команду — тому входу, с которого она пришла.
///
/// Действиям автомата всё равно, ждёт ли клиент в вызове (`CMD`) или забирает
/// исход позже (`ASK`): ответ один и тот же, дроп без ответа у обоих —
/// «узел не ответил».
#[derive(Debug)]
pub(super) enum Answer<'a> {
    /// Команда с входа `CMD`.
    Call(Served<'a, Request, Reply>),
    /// Команда с входа `ASK`.
    Ask(Asked<'a, Request, Reply>),
}

impl Answer<'_> {
    /// Исход команды — клиенту.
    pub(super) fn respond(self, reply: Reply) {
        match self {
            Self::Call(served) => served.respond(reply),
            Self::Ask(asked) => asked.respond(reply),
        }
    }
}

/// Мест ожидания входа `CMD` на `clients` его клиентов — параметр `PARKED`
/// фрагмента `OTA_FRAG`; число клиентов знает только compose-site.
///
/// Клиенты — заранее известный набор, по одному на транспорт: один держит
/// вызов, ждут остальные, так что мест на одно меньше клиентов — тогда
/// ждущие не будят друг друга по кругу (`sync_request::PARKED_CALLERS`,
/// документация). Не меньше одного: ноль мест не компилируется, а одного
/// хватает и на двух клиентов — в том числе тех, что проект без протокола
/// подключит к входу сам.
///
/// ```
/// use domain::ota::cmd_parked;
///
/// assert_eq!(cmd_parked(0), 1);
/// assert_eq!(cmd_parked(2), 1);
/// assert_eq!(cmd_parked(6), 5);
/// ```
#[must_use]
pub const fn cmd_parked(clients: usize) -> usize {
    if clients > 2 { clients - 1 } else { 1 }
}

/// Команда приёма не от владельца: `Busy`, приём владельца — и его срок — как
/// были.
fn busy(s: Receiving, client: ClientId, served: Answer<'_>) -> Receiving {
    warn!(
        "ota: команда клиента {} отвергнута: идёт приём клиента {}",
        client.0, s.owner.0
    );
    served.respond(Err(Rejection::Busy));
    s
}

/// Код отказа для хоста по ошибке приёма.
fn download_rejection(err: &DownloadError) -> Rejection {
    match err {
        DownloadError::Empty | DownloadError::TooLong { .. } => Rejection::Length,
        DownloadError::TooMuchData { .. }
        | DownloadError::Incomplete { .. }
        | DownloadError::OutOfOrder { .. } => Rejection::Transfer,
        // `Flash` и незнакомые варианты (перечисление `non_exhaustive`)
        // сообщаются как отказ устройства, а не прячутся.
        _ => Rejection::Device,
    }
}

/// Код отказа для хоста по ошибке применения.
fn update_rejection(err: &UpdateError) -> Rejection {
    match err {
        UpdateError::TooLong { .. } | UpdateError::Truncated { .. } => Rejection::Length,
        UpdateError::Busy => Rejection::Busy,
        UpdateError::Rollback { .. } => Rejection::Rollback,
        UpdateError::Layout { .. } => Rejection::Layout,
        // `Flash` и незнакомые варианты (перечисление `non_exhaustive`).
        _ => Rejection::Device,
    }
}

// {%- if graph == "true" %}
// Узел объявлен рядом со своей задачей, как `HEARTBEAT_FRAG` в
// `domain::heartbeat`; compose-site (`crates-cross/app/src/graph.rs`) только
// перечисляет фрагмент, называет узел (`as OTA`) и кормит его входом.
// {%- else %}
// Узел объявлен рядом со своей задачей; задача модуля
// `crates-cross/app/src/tasks/ota.rs` кормит его входом.
// {%- endif %}
// Слова DSL и зачем они здесь:
//
// - `boot: inputs: …` — собственная привязка фрагмента.
// {%- if graph == "true" %}
//   Compose-site пишет
//   `fragments: [::domain::OTA_FRAG<…> as OTA = ::domain::ota::OtaWiring { … }]`,
//   `spawn_all` связывает её первой строкой пролога, инициализаторы слотов
//   ниже читают её поля.
// {%- else %}
//   Без графа её никто не заполняет: задача `crates-cross/app/src/tasks/ota.rs`
//   собирает `OtaCtx` сама, слоты — её локальные переменные с теми же
//   начальными значениями.
// {%- endif %}
// - `CONFIRMED` — подтверждён ли образ в этом запуске (см. `begin`). Начальное
//   значение — из свойств образа бинарника (константа `IMAGE` типа
//   `app::version::Image` в `crates-cross/app/src/main.rs`{% if test_image == "true" %} и
//   `src/bin/app-unconfirmed.rs`{% endif %}): у рабочего `false`{% if test_image == "true" %}; у тестового
//   `app-unconfirmed` (`cargo xtask test host-target`) — `true`, он
//   себя не подтверждает, и следующий сброс его откатывает{% endif %}.
// - `request: [CMD: …]` — запрос с ответом (`sync-request`), а не очередь:
//   протокол отвечает хосту исходом именно своей команды.
// {%- if graph == "true" %}
//   Клиент — `Client`: requester из аксессора графа `ota_cmd_requester()` и
//   номер транспорта, который раздаёт compose-site.
// {%- else %}
//   Клиент — `Client`: requester входа и номер транспорта, которые раздаёт
//   `crates-cross/app/src/tasks/ota.rs`.
// {%- endif %}
// - `serves: [ASK: …]` — тот же вход для клиента, которому нельзя ждать
//   исхода в вызове (узел протокола, обслуживающий свой канал): он объявляет
//   ребро `asks: [OTA.ASK: …; квота tag …]`, шлёт команду не дожидаясь и
//   забирает исход из `reply()`. Очередь — сумма квот рёбер (считает
//   `supervisor`); без спрашивающих она пустая, и `ASK` молчит.
// - Ёмкость `CMD` и `ASK` — `CMD_BYTES`, литералом: макрос другого не
//   принимает.
// {%- if graph != "true" %}
//   Её же берут `static` входов (`crates-cross/app/src/tasks/ota.rs`).
// {%- endif %}
//   Менять все места вместе.
// - `PARKED` — мест ожидания у `CMD` (`parked`): клиентов входа знает не
//   фрагмент, а compose-site — `cmd_parked` от их числа из генерации. Без
//   этого мест было бы `sync_request::PARKED_CALLERS`, и ждущие сверх него
//   будили бы друг друга по кругу.
// - `<F, R>` — адаптер обновления над разделами платы
//   (`Updater<bsp::FirmwareFlash>`) и порт перезагрузки: их типов `domain` не
//   видит.
// - `local` на `FLASH` — `Updater` над разделами `!Send` (держит
//   `&'static Mutex<NoopRawMutex, …>`), а обычный слот — `static`, которому
//   нужен `Send`. `LocalResourceSlot` снимает требование, взамен обязывая
//   держать значение на исполнителе, который спавнит задачи (то же
//   допущение, что `NoopRawMutex` у `Lock`). Цена на compose-site —
//   фича `supervisor/local-resources` и `unsafe`-блок на каждый
//   `local`-`static`.
// {%- if graph == "true" %}
//   Подробнее — `docs/modules/app-graph.md`, «OTA: узел уже в графе, вам
//   остаётся канал».
// - Без `watchdog:` намеренно — см. `task`.
// {%- endif %}
// - `ready` — образ подтверждён (или попытка была): стирания `STATE` при
//   старте в этом запуске уже не будет (см. `begin`){% if has_usb == "true" %}; ждёт этого узел `USB`{% endif %}.
supervisor::supervisor_fragment! {
    name: OTA_FRAG<F, R, const PARKED: usize>;
    boot: inputs: struct $crate::ota::OtaWiring;

    node resources: [
            /// Адаптер обновления поверх разделов флеша (`Updater`).
            FLASH: local F = inputs.flash,
            /// Перезагрузка МК (`Reboot`) — для команды `Reboot`.
            REBOOT: R = inputs.reboot,
            /// Версия этой прошивки (свойства образа приложения): образ не новее
            /// неё отвергается на `Finish` — защита от отката.
            RUNNING_VERSION: u32 = inputs.running_version,
            /// Подтверждён ли образ в этом запуске; начальное — от приложения
            /// (свойства образа бинарника, см. выше).
            CONFIRMED: bool = inputs.confirmed
        ],
        // Литерал, а не `CMD_BYTES`: макрос принимает здесь только число. Равен ему.
        request: [CMD: $crate::ota::Request -> $crate::ota::Reply; 144 parked PARKED],
        serves: [ASK: $crate::ota::Request -> $crate::ota::Reply; 144],
        ready,
        ctx: $crate::ota::OtaCtx,
        task: $crate::ota::task;
}

// {%- if graph == "true" %}
/// Узел `OTA`: отвечает на команды приёма образа, пока жив граф.
// {%- else %}
/// Узел `OTA`: отвечает на команды приёма образа.
// {%- endif %}
///
/// Не выходит никогда: каждая команда получает ответ — исход или код
/// отказа, — и узел ждёт следующую.
// {%- if graph == "true" %}
///
/// Без `watchdog:` намеренно — по тому же правилу, что у
/// [`heartbeat::task`](crate::heartbeat::task): отметка ставится после
/// реального прогресса, а команды может не быть всю жизнь платы. Аппаратный
/// сторож кормит тикер графа, а не какой-то узел.
// {%- endif %}
pub async fn task<D, S, R, const PARKED: usize>(
    ctx: OtaCtx<'_, Updater<Partitions<D, S>>, R, PARKED>,
) where
    D: NorFlash,
    S: NorFlash,
    R: Reboot,
{
    let mut ota = Ota::new(
        ctx.flash,
        ctx.reboot,
        ctx.running_version,
        ctx.confirmed,
        ctx.cmd,
        ctx.ask,
        ctx.ready,
    );
    let mut state = State::Idle(Idle);
    ota.begin().await;
    loop {
        // Команда первой, как первая строка `listen`: срок — молчание хоста,
        // и уже пришедшая команда его не нарушила. `Receiving::start` сам
        // сверяет `until`, так что обогнавшая срок команда всё равно
        // обработана по своему событию.
        let until = state.deadline();
        let (request, served) = match until {
            Some(until) => match select(ota.command(), deadline(until)).await {
                Either::First(turn) => turn,
                Either::Second(()) => {
                    let State::Receiving(s) = state else {
                        unreachable!("срок только у Receiving")
                    };
                    state = s.expired();
                    continue;
                }
            },
            None => ota.command().await,
        };
        let Request { client, command } = request;
        if let Command::Reboot = command {
            // `Reboot` — из любого состояния: ответ, пауза на его отправку,
            // сброс. Состояние не меняется.
            ota.reboot(served).await;
            continue;
        }
        state = state.on_command(&mut ota, client, command, served).await;
    }
}

#[cfg(test)]
mod fake {
    //! Разделы обновления в памяти под настоящим [`Updater`] — общие для
    //! `download`, `update` и узла `ota`.
    //!
    //! Флеш — `MemFlash` (придирчивый, как NOR: кратность слову, запись только
    //! «единицы в нули»), а поверх — [`Probe`]: журнал операций и отказы там,
    //! где их надо разыграть (запись для bootloader'а, магия `STATE`,
    //! стирание `STATE`), и долгое стирание `STATE` на таймере `MockDriver`.
    //! Всё остальное — `embassy-boot` и адаптер — настоящее.
    //!
    //! Флеш в памяти не ждёт — future готовы с первого `poll`, — поэтому тестам
    //! хватает однопоточного `#[tokio::test]`. Исключение —
    //! [`confirm_takes`](Updater::confirm_takes): стирание `STATE` ждёт таймер.

    use adapters_hw::flash::fake::MemFlash;
    use embassy_time::{Duration, Timer};
    use embedded_storage_async::nor_flash::{ErrorType, NorFlash, NorFlashErrorKind, ReadNorFlash};
    use ports::{Partitions, PendingImage};

    use crate::ota::Updater;
    use crate::ota::update::{LAYOUT_BYTES, VERSION_BYTES};

    /// Отпечаток раскладки разделов тестов — его отдаёт `layout()`; образы
    /// тестов несут его в хвосте, кроме тех, что разыгрывают чужую раскладку.
    pub(crate) const LAYOUT: u32 = 0x5EED_1A70;

    /// Подпись, которую тесты кладут в `Start`; приложение её не проверяет, а
    /// оставляет bootloader'у — важно, что дошла именно она.
    pub(super) const SIGNATURE: [u8; 64] = [9; 64];

    /// Страница стирания: кратна любому слову STM32 (до 32 байт) и вмещает
    /// запись для bootloader'а (`PendingImage::LEN`, 96 байт).
    const PAGE: usize = 128;

    /// Объём `DFU`: самый длинный образ тестов (1024 байта) и запасная
    /// страница за ним.
    const DFU: usize = 1024 + PAGE;

    /// Магия `embassy-boot`: образ подтверждён.
    const BOOT_MAGIC: u8 = 0xD0;

    /// Магия `embassy-boot`: обмен на следующем сбросе.
    const SWAP_MAGIC: u8 = 0xF0;

    /// Раздел `DFU` со словом `W`.
    pub(super) type Dfu<const W: usize> = Probe<MemFlash<DFU, W, PAGE>>;

    /// Раздел `STATE`: страница на магию и журнал.
    pub(super) type State = Probe<MemFlash<PAGE, 8, PAGE>>;

    /// Настоящий адаптер обновления над разделами в памяти, слово `DFU` — `W`.
    pub(crate) type FakeFlash<const W: usize = 8> = Updater<Partitions<Dfu<W>, State>>;

    /// Флеш с журналом операций и отказами по заказу.
    pub(super) struct Probe<F> {
        flash: F,
        /// Запись по этому смещению и дальше отказывает.
        refuse_from: Option<u32>,
        /// Магии, запись которых по смещению 0 отказывает.
        refuse_magic: Vec<u8>,
        /// Стирание отказывает.
        refuse_erase: bool,
        /// Сколько длится стирание.
        erase_takes: Option<Duration>,
        /// Удачные записи: смещение и длина.
        writes: Vec<(u32, usize)>,
        /// Сколько раз стирали.
        erases: usize,
        /// Магии, которые пытались записать по смещению 0, и удалось ли.
        magics: Vec<(u8, bool)>,
    }

    impl<F> Probe<F> {
        const fn new(flash: F) -> Self {
            Self {
                flash,
                refuse_from: None,
                refuse_magic: Vec::new(),
                refuse_erase: false,
                erase_takes: None,
                writes: Vec::new(),
                erases: 0,
                magics: Vec::new(),
            }
        }
    }

    impl<F: ErrorType<Error = NorFlashErrorKind>> ErrorType for Probe<F> {
        type Error = NorFlashErrorKind;
    }

    impl<F: ReadNorFlash + ErrorType<Error = NorFlashErrorKind>> ReadNorFlash for Probe<F> {
        const READ_SIZE: usize = F::READ_SIZE;

        async fn read(&mut self, offset: u32, bytes: &mut [u8]) -> Result<(), Self::Error> {
            self.flash.read(offset, bytes).await
        }

        fn capacity(&self) -> usize {
            self.flash.capacity()
        }
    }

    impl<F: NorFlash + ErrorType<Error = NorFlashErrorKind>> NorFlash for Probe<F> {
        const WRITE_SIZE: usize = F::WRITE_SIZE;
        const ERASE_SIZE: usize = F::ERASE_SIZE;

        async fn erase(&mut self, from: u32, to: u32) -> Result<(), Self::Error> {
            if let Some(takes) = self.erase_takes {
                Timer::after(takes).await;
            }
            if self.refuse_erase {
                return Err(NorFlashErrorKind::Other);
            }
            self.erases += 1;
            self.flash.erase(from, to).await
        }

        async fn write(&mut self, offset: u32, bytes: &[u8]) -> Result<(), Self::Error> {
            let magic = (offset == 0)
                .then(|| bytes.first().copied())
                .flatten()
                .filter(|magic| bytes.iter().all(|b| b == magic));
            let refused = magic.is_some_and(|magic| self.refuse_magic.contains(&magic))
                || self.refuse_from.is_some_and(|from| offset >= from);
            if let Some(magic) = magic {
                self.magics.push((magic, !refused));
            }
            if refused {
                return Err(NorFlashErrorKind::Other);
            }
            self.flash.write(offset, bytes).await?;
            self.writes.push((offset, bytes.len()));
            Ok(())
        }
    }

    /// Наблюдения и отказы — у того же адаптера, что видит узел.
    impl<const W: usize> FakeFlash<W> {
        /// Содержимое раздела `DFU`.
        pub(super) fn memory(&mut self) -> &mut Vec<u8> {
            &mut self.partitions().dfu.flash.mem
        }

        /// Удачные записи в `DFU`: смещение и длина.
        pub(super) fn writes(&mut self) -> &[(u32, usize)] {
            &self.partitions().dfu.writes
        }

        /// Готовили ли раздел — стирали ли `DFU`.
        pub(super) fn prepared(&mut self) -> bool {
            self.partitions().dfu.erases > 0
        }

        /// Сколько раз готовили раздел: `prepare` стирает `capacity` и
        /// запасную страницу — по стиранию на страницу.
        pub(super) fn prepares(&mut self) -> usize {
            let pages = self.capacity() as usize / PAGE + 1;
            self.partitions().dfu.erases / pages
        }

        /// Запись для bootloader'а за `capacity`: подпись и длина.
        pub(super) fn recorded(&mut self) -> Option<([u8; 64], u32)> {
            let at = self.capacity() as usize;
            let bytes: [u8; PendingImage::LEN] = self.memory()[at..at + PendingImage::LEN]
                .try_into()
                .expect("за capacity есть место под запись");
            PendingImage::from_bytes(&bytes).map(|record| (record.signature, record.len))
        }

        /// Помечен ли обмен разделов (`mark_updated`).
        pub(super) fn swap_requested(&mut self) -> bool {
            self.partitions().state.flash.mem[..8] == [SWAP_MAGIC; 8]
        }

        /// Сколько раз помечали обмен (запись магии `Swap` прошла).
        pub(super) fn magics_swap(&mut self) -> usize {
            let magics = &self.partitions().state.magics;
            magics.iter().filter(|m| **m == (SWAP_MAGIC, true)).count()
        }

        /// Сколько раз образ подтвердили (запись магии `Boot` прошла).
        pub(super) fn booted(&mut self) -> usize {
            let magics = &self.partitions().state.magics;
            magics.iter().filter(|m| **m == (BOOT_MAGIC, true)).count()
        }

        /// Сколько раз пытались подтвердить образ — удачно или нет.
        pub(super) fn confirm_attempts(&mut self) -> usize {
            let magics = &self.partitions().state.magics;
            magics.iter().filter(|(m, _)| *m == BOOT_MAGIC).count()
        }

        /// В `STATE` — образ, ждущий обмена: раздел занят.
        pub(super) fn set_busy(&mut self) {
            self.partitions().state.flash.mem[..8].fill(SWAP_MAGIC);
        }

        /// Подтверждение образа отказывает (`refuse`) или снова проходит:
        /// запись магии `Boot` не проходит после стирания `STATE`.
        pub(super) fn refuse_confirm(&mut self, refuse: bool) {
            let magics = &mut self.partitions().state.refuse_magic;
            magics.retain(|m| *m != BOOT_MAGIC);
            if refuse {
                magics.push(BOOT_MAGIC);
            }
        }

        /// Стирание `STATE` отказывает: подтверждение и пометка обмена не
        /// трогают раздел — образ, ждущий обмена, так и остаётся.
        pub(super) fn refuse_state_erase(&mut self) {
            self.partitions().state.refuse_erase = true;
        }

        /// Пометка обмена отказывает: запись магии `Swap` не проходит.
        pub(super) fn refuse_swap(&mut self) {
            self.partitions().state.refuse_magic.push(SWAP_MAGIC);
        }

        /// Запись в `DFU` отказывает с `offset` и дальше.
        pub(super) fn refuse_writes_from(&mut self, offset: u32) {
            self.partitions().dfu.refuse_from = Some(offset);
        }

        /// Запись для bootloader'а (за `capacity`) отказывает.
        pub(super) fn refuse_record(&mut self) {
            let capacity = self.capacity();
            self.refuse_writes_from(capacity);
        }

        /// Стирание `STATE` — то, чем подтверждается образ, — длится `takes`.
        pub(super) fn confirm_takes(&mut self, takes: Duration) {
            self.partitions().state.erase_takes = Some(takes);
        }
    }

    /// Адаптер над чистыми разделами со словом `W` и самым длинным образом
    /// `capacity` (кратно 128 и не больше 1024).
    pub(super) fn flash_with_word<const W: usize>(capacity: u32) -> FakeFlash<W> {
        Updater::new(Partitions {
            dfu: Probe::new(MemFlash::erased()),
            state: Probe::new(MemFlash::erased()),
            capacity,
            layout: LAYOUT,
        })
    }

    /// То же со словом 8.
    pub(super) fn flash(capacity: u32) -> FakeFlash {
        flash_with_word(capacity)
    }

    /// Раздел, в котором уже лежит образ длиной `len` с отпечатком [`LAYOUT`]
    /// и версией `incoming` в хвосте; самый длинный образ — 128 байт.
    pub(super) fn with_image(len: u32, incoming: u32) -> FakeFlash {
        let mut flash = flash(128);
        let version = (len - VERSION_BYTES) as usize;
        let memory = flash.memory();
        memory[version - LAYOUT_BYTES as usize..version].copy_from_slice(&LAYOUT.to_le_bytes());
        memory[version..len as usize].copy_from_slice(&incoming.to_le_bytes());
        flash
    }
}

#[cfg(test)]
pub(crate) mod tests {
    //! Host-тесты узла `OTA` на фейках портов: сценарии команд через входы
    //! `CMD`/`ASK` и стенды узла для тестов протоколов (`live`, `cmd`,
    //! `serving`).

    use core::pin::{Pin, pin};
    use core::task::{Context, Poll, Waker};

    use embassy_time::{Duration, MockDriver};
    use ports::Reboot;

    use crate::ota::commands::{Chunk, Progress, Rejection};
    use crate::ota::{Command, Reply};

    // {%- if has_modbus == "true" or has_protobuf == "true" or has_cli == "true" or net_services contains "mqtt" or net_services contains "rest" or net_services contains "ssh" %}
    use super::Client;
    // {%- endif %}

    use super::{ClientId, REBOOT_GRACE, Request, SESSION_TIMEOUT, task};
    /// Отпечаток раскладки фейка — образы тестов узла и протоколов несут его
    /// в хвосте.
    pub(crate) use crate::ota::fake::LAYOUT;
    use crate::ota::fake::{FakeFlash, SIGNATURE, flash};
    use crate::ota::update::{LAYOUT_BYTES, VERSION_BYTES};
    use crate::ota::version::pack;

    /// Версия «этой прошивки» во входе узла — у стендов протоколов тоже.
    const RUNNING_VERSION: u32 = pack(1, 2, 3);

    /// Версия в хвосте образов [`image`] — новее [`RUNNING_VERSION`].
    const NEWER: u32 = pack(1, 2, 4);

    /// Сброс МК в тесте — паника с узнаваемым текстом: `reboot` не
    /// возвращается, а `#[should_panic]` ловит именно её.
    struct FakeReboot;

    impl Reboot for FakeReboot {
        fn reboot(&mut self) -> ! {
            panic!("МК сброшен")
        }
    }

    /// Шаг сценария: запрос узлу, пауза, которую узел видит, или пауза, которую
    /// он не успел увидеть (время ушло, а узел не опрошен).
    #[derive(Debug, Clone)]
    enum Step {
        Send(Request),
        /// Та же команда, но через вход `ASK`.
        Ask(Request),
        Wait(Duration),
        Skip(Duration),
    }

    // {%- if has_modbus == "true" or has_protobuf == "true" or net_services contains "mqtt" %}
    /// Узел OTA, который тест крутит сам: `poll` по очереди с запросом.
    pub(crate) type Node = Pin<Box<dyn Future<Output = ()>>>;

    /// Узел OTA на фейке флеша и его клиент — для тестов
    // {%- if has_modbus == "true" %}
    /// обработчиков протоколов (`modbus`)
    // {%- endif %}
    // {%- if has_modbus == "true" and has_protobuf == "true" %}
    /// и
    // {%- endif %}
    // {%- if has_protobuf == "true" %}
    /// узла `protobuf`
    // {%- endif %}
    // {%- if net_services contains "mqtt" %}{% if has_modbus == "true" or has_protobuf == "true" %}
    /// и
    // {%- endif %}{% endif %}
    // {%- if net_services contains "mqtt" %}
    /// сессии MQTT (`mqtt::serve`)
    // {%- endif %}
    /// — стенд,
    /// флеш и слоты живут до конца процесса, как в прошивке: клиент —
    /// `'static`, как у `ota_cmd_requester()`.
    pub(crate) fn live() -> (Client, Node) {
        let stand = Box::leak(Box::new(
            supervisor::stand!(OTA_FRAG<FakeFlash, FakeReboot, 1>; QUEUE_ASK = 1),
        ));
        let node = task(stand.ctx(
            Box::leak(Box::new(flash(128))),
            Box::leak(Box::new(FakeReboot)),
            Box::leak(Box::new(RUNNING_VERSION)),
            Box::leak(Box::new(false)),
        ));
        (
            Client::new(stand.cmd.requester(), ClientId(0)),
            Box::pin(node),
        )
    }
    // {%- endif %}

    // {%- if net_services contains "rest" or net_services contains "ssh" or has_modbus == "true" or has_cli == "true" %}
    /// Вход `CMD` узла OTA без самого узла и его клиент — для тестов
    /// протоколов (`http`, `ssh`, `modbus`) и консоли: на команду отвечает тест, и исходом,
    /// которого узел не даёт, и молчанием (сервер берёт запрос и бросает его).
    pub(crate) fn cmd() -> (Client, supervisor::Server<'static, Request, Reply>) {
        let stand = Box::leak(Box::new(
            supervisor::stand!(OTA_FRAG<FakeFlash, FakeReboot, 1>; QUEUE_ASK = 1),
        ));
        (
            Client::new(stand.cmd.requester(), ClientId(0)),
            stand.cmd.server().expect("сервер CMD стенда свободен"),
        )
    }
    // {%- endif %}

    // {%- if has_canopen == "true" %}
    /// Узел OTA на фейке флеша, чей вход `ASK` — `ask`: сервер ребра из стенда
    /// узла-клиента (`stand.ota_ask` у фрагмента с `asks: [OTA.ASK ..]`).
    pub(crate) fn serving<'a>(
        ask: supervisor::AskServer<'a, Request, Reply>,
    ) -> Pin<Box<dyn Future<Output = ()> + 'a>> {
        let stand = Box::leak(Box::new(
            supervisor::stand!(OTA_FRAG<FakeFlash, FakeReboot, 1>; QUEUE_ASK = 1),
        ));
        Box::pin(task(super::OtaCtx::<'_, _, _, 1> {
            flash: Box::leak(Box::new(flash(128))),
            reboot: Box::leak(Box::new(FakeReboot)),
            running_version: Box::leak(Box::new(RUNNING_VERSION)),
            confirmed: Box::leak(Box::new(false)),
            cmd: stand.cmd.server().expect("сервер CMD стенда свободен"),
            ask,
            ready: supervisor::Readiness::new(|| {}),
            index: 0,
        }))
    }
    // {%- endif %}

    /// Команда от клиента 0 — единственного транспорта.
    fn send(command: Command) -> Step {
        Step::Send(Request {
            client: ClientId(0),
            command,
        })
    }

    /// Та же команда от клиента `client`.
    fn by(client: u8, step: Step) -> Step {
        let Step::Send(request) = step else {
            panic!("by() — только для команды");
        };
        Step::Send(Request {
            client: ClientId(client),
            ..request
        })
    }

    /// Та же команда через вход `ASK`.
    fn asked(step: Step) -> Step {
        let Step::Send(request) = step else {
            panic!("asked() — только для команды");
        };
        Step::Ask(request)
    }

    /// Опрашивает `reply` по очереди с узлом, пока исход не придёт.
    fn await_reply<T>(
        mut node: Pin<&mut impl Future<Output = ()>>,
        mut reply: Pin<&mut impl Future<Output = T>>,
    ) -> T {
        (0..16)
            .find_map(|_| match poll(reply.as_mut()) {
                Poll::Ready(reply) => Some(reply),
                Poll::Pending => {
                    assert!(poll(node.as_mut()).is_pending(), "узел не выходит");
                    None
                }
            })
            .expect("узел ответил за конечное число шагов")
    }

    fn poll<T>(fut: Pin<&mut impl Future<Output = T>>) -> Poll<T> {
        fut.poll(&mut Context::from_waker(Waker::noop()))
    }

    /// Гоняет узел по сценарию и возвращает ответы на команды по порядку.
    /// `confirmed` — слот снаружи: образ, который себя не подтверждает,
    /// приходит с `true`, а сценарий видит, чем узел его оставил.
    fn run_on(flash: &mut FakeFlash, confirmed: &mut bool, steps: Vec<Step>) -> Vec<Reply> {
        MockDriver::get().reset();
        let stand = supervisor::stand!(OTA_FRAG<FakeFlash, FakeReboot, 1>; QUEUE_ASK = 1);
        let (mut reboot, mut version) = (FakeReboot, RUNNING_VERSION);
        let requester = stand.cmd.requester();
        let mut asker = stand.ask();
        let mut node = pin!(task(stand.ctx(flash, &mut reboot, &mut version, confirmed)));
        assert!(poll(node.as_mut()).is_pending(), "узел ждёт команду");

        let mut replies = Vec::new();
        for step in steps {
            let reply = match step {
                Step::Send(request) => await_reply(node.as_mut(), pin!(requester.request(request)))
                    .expect("узел ответил"),
                Step::Ask(request) => {
                    asker.try_call((), request).expect("очередь ASK свободна");
                    await_reply(node.as_mut(), pin!(asker.reply()))
                        .outcome
                        .expect("узел ответил")
                }
                Step::Wait(pause) => {
                    MockDriver::get().advance(pause);
                    assert!(poll(node.as_mut()).is_pending(), "узел не выходит");
                    continue;
                }
                Step::Skip(pause) => {
                    MockDriver::get().advance(pause);
                    continue;
                }
            };
            replies.push(reply);
        }
        replies
    }

    fn run(flash: &mut FakeFlash, steps: Vec<Step>) -> Vec<Reply> {
        run_on(flash, &mut false, steps)
    }

    /// `Start` с подписью — как его шлёт любой хост.
    #[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
    fn start(len: usize) -> Step {
        send(Command::Start {
            len: len as u32,
            signature: Some(SIGNATURE),
        })
    }

    /// `Start` без подписи — его узел отвергает.
    #[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
    fn unsigned_start(len: usize) -> Step {
        send(Command::Start {
            len: len as u32,
            signature: None,
        })
    }

    fn chunk(offset: u32, data: &[u8]) -> Step {
        send(Command::Chunk(
            Chunk::new(offset, data).expect("кусок помещается"),
        ))
    }

    /// Образ кусками по 13 байт — некратно ни слову, ни друг другу.
    #[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
    fn chunks(image: &[u8]) -> impl Iterator<Item = Step> {
        image
            .chunks(13)
            .enumerate()
            .map(|(i, data)| chunk((i * 13) as u32, data))
    }

    fn finish() -> Step {
        send(Command::Finish)
    }

    /// Образ, как его собирает `cargo xtask build`: хвост — отпечаток
    /// раскладки фейка ([`LAYOUT`]) и версия [`NEWER`]; без них `Finish`
    /// отвергался бы.
    fn image(len: usize) -> Vec<u8> {
        versioned_image(len, NEWER)
    }

    /// Образ с отпечатком [`LAYOUT`] и версией `version` в хвосте.
    #[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
    fn versioned_image(len: usize, version: u32) -> Vec<u8> {
        let mut image: Vec<u8> = (0..len).map(|i| (i * 3) as u8).collect();
        let at = len - VERSION_BYTES as usize;
        image[at - LAYOUT_BYTES as usize..at].copy_from_slice(&LAYOUT.to_le_bytes());
        image[at..].copy_from_slice(&version.to_le_bytes());
        image
    }

    /// Полный приём образа: `Start`, куски, `Finish`.
    fn update(image: &[u8]) -> Vec<Step> {
        let mut steps = vec![start(image.len())];
        steps.extend(chunks(image));
        steps.push(finish());
        steps
    }

    /// Счастливый путь: раздел подготовлен один раз, каждый кусок
    /// подтверждён принятой длиной, подпись из `Start` и длина доехали до
    /// адаптера записью для bootloader'а, и только после неё помечен обмен;
    /// хост услышал `Ready`.
    #[test]
    fn applies_an_image_and_replies_to_every_command() {
        let image = image(64);
        let mut flash = flash(1024);

        let replies = run(&mut flash, update(&image));

        assert_eq!(flash.prepares(), 1, "раздел подготовлен один раз");
        assert_eq!(&flash.memory()[..64], &image[..]);
        assert_eq!(flash.recorded(), Some((SIGNATURE, 64)));
        assert!(flash.swap_requested(), "обмен разделов запрошен");
        assert_eq!(
            replies,
            vec![
                Ok(Progress::Started),
                Ok(Progress::Received(13)),
                Ok(Progress::Received(26)),
                Ok(Progress::Received(39)),
                Ok(Progress::Received(52)),
                Ok(Progress::Received(64)),
                Ok(Progress::Ready),
            ]
        );
    }

    /// Образ подтверждается при старте узла — до первой команды: иначе
    /// bootloader откатил бы обновление на следующем сбросе.
    #[test]
    fn confirms_the_running_image_before_the_first_command() {
        let mut flash = flash(128);

        run(&mut flash, vec![]);

        assert_eq!(flash.booted(), 1);
        assert!(!flash.prepared(), "раздел не тронут");
    }

    /// Готовность — только когда подтверждение закончилось: узлу, который её
    /// ждёт, нельзя стартовать, пока подтверждение стирает `STATE`.
    #[test]
    fn is_ready_only_after_the_confirmation() {
        MockDriver::get().reset();
        let mut flash = flash(128);
        flash.confirm_takes(Duration::from_millis(1000));
        // Свой `stand!` на тест: готовность — `static` на месте вызова.
        let stand = supervisor::stand!(OTA_FRAG<FakeFlash, FakeReboot, 1>; QUEUE_ASK = 1);
        let (mut reboot, mut version, mut confirmed) = (FakeReboot, RUNNING_VERSION, false);
        let mut node = pin!(task(stand.ctx(
            &mut flash,
            &mut reboot,
            &mut version,
            &mut confirmed
        )));

        assert!(poll(node.as_mut()).is_pending());
        assert!(!stand.is_ready(), "подтверждение ещё идёт");

        MockDriver::get().advance(Duration::from_millis(1000));
        assert!(poll(node.as_mut()).is_pending());
        assert!(stand.is_ready());
    }

    /// И после неудачного: иначе отказ флеша навсегда задержал бы узлы,
    /// которые ждут готовности.
    #[test]
    fn is_ready_after_a_failed_confirmation() {
        let mut flash = flash(128);
        flash.refuse_confirm(true);
        let stand = supervisor::stand!(OTA_FRAG<FakeFlash, FakeReboot, 1>; QUEUE_ASK = 1);
        let (mut reboot, mut version, mut confirmed) = (FakeReboot, RUNNING_VERSION, false);
        let mut node = pin!(task(stand.ctx(
            &mut flash,
            &mut reboot,
            &mut version,
            &mut confirmed
        )));

        assert!(poll(node.as_mut()).is_pending());
        assert!(stand.is_ready());
    }

    /// Образ с `confirmed = true` во входе себя не подтверждает ни при
    /// старте, ни на `Start`, и следующий сброс его откатывает.
    // {%- if test_image == "true" %}
    /// Так приходит тестовый образ `app-unconfirmed`; на этом стоит тест
    /// отката `host-target`.
    // {%- endif %}
    #[test]
    fn an_image_started_confirmed_never_confirms_itself() {
        let mut confirmed = true;
        let mut flash = flash(128);

        run_on(&mut flash, &mut confirmed, vec![start(1000)]);

        assert_eq!(flash.confirm_attempts(), 0);
    }

    /// Не удалось подтвердить при старте — узел пробует ещё раз на каждом
    /// `Start`, и только на нём: повтор стоит стирания `STATE`.
    #[test]
    fn a_failed_confirmation_is_retried_on_the_next_start() {
        let mut confirmed = false;
        let mut flash = flash(128);
        flash.refuse_confirm(true);

        // Образ длиннее раздела — отвергнут, но подтверждение пробовалось.
        run_on(
            &mut flash,
            &mut confirmed,
            vec![start(1000), chunk(0, &[0; 8])],
        );

        assert_eq!(
            flash.confirm_attempts(),
            2,
            "при старте и на Start, на куске — нет"
        );
        assert!(!confirmed);
    }

    /// Отказ подтверждения не мешает принять образ.
    #[test]
    fn a_failed_confirmation_does_not_block_an_update() {
        let mut flash = flash(128);
        flash.refuse_confirm(true);

        let replies = run(&mut flash, vec![start(8), chunk(0, &image(8)), finish()]);

        assert_eq!(replies.last(), Some(&Ok(Progress::Ready)), "образ принят");
    }

    /// Принятый образ отменяет повтор подтверждения: после `mark_updated`
    /// `mark_booted` снял бы запрос на обмен, о котором хост уже услышал
    /// `Ready`.
    #[test]
    fn an_accepted_image_is_never_followed_by_a_confirmation() {
        let mut flash = flash(128);
        flash.refuse_confirm(true);

        run(
            &mut flash,
            vec![start(16), chunk(0, &image(16)), finish(), start(16)],
        );

        assert!(flash.swap_requested(), "обмен запрошен");
        assert_eq!(
            flash.confirm_attempts(),
            2,
            "при старте и на первом Start, на втором — нет"
        );
    }

    /// Занятость — до стирания: адаптер и так отказал бы в `prepare`, но
    /// хост услышал бы `Device` вместо «перезагрузи».
    #[test]
    fn replies_busy_before_touching_the_partition() {
        let mut flash = flash(128);
        // Образ ждёт обмена, а подтверждение не трогает `STATE` (стирание
        // отказывает): раздел занят и после старта узла.
        flash.set_busy();
        flash.refuse_state_erase();

        let replies = run(&mut flash, vec![start(8)]);

        assert_eq!(replies, vec![Err(Rejection::Busy)]);
        assert!(!flash.prepared(), "раздел не должен быть стёрт");
    }

    /// После `Ready` новый образ — `Busy`, а не стирание раздела с образом,
    /// ждущим обмена; узел по-прежнему ждёт `Reboot`.
    #[test]
    fn a_start_after_ready_is_busy_and_keeps_the_pending_image() {
        let mut flash = flash(128);
        let image = image(16);

        let replies = run(
            &mut flash,
            vec![
                start(16),
                chunk(0, &image),
                finish(),
                start(16),
                chunk(0, &[0; 8]),
            ],
        );

        assert_eq!(
            &replies[3..],
            &[Err(Rejection::Busy), Err(Rejection::NoSession)],
            "Start отвергнут, приёма нет"
        );
        assert_eq!(
            &flash.memory()[..16],
            &image[..],
            "образ, ждущий обмена, цел"
        );
    }

    /// Повтор `Finish` владельца после `Ready` (ответ потерялся) — снова
    /// `Ready`, и ничего не записано повторно: обмен помечен один раз.
    #[test]
    fn a_repeated_finish_of_the_owner_is_ready_again() {
        let mut flash = flash(128);

        let replies = run(
            &mut flash,
            vec![start(16), chunk(0, &image(16)), finish(), finish()],
        );

        assert_eq!(
            &replies[2..],
            &[Ok(Progress::Ready), Ok(Progress::Ready)],
            "повтор получил тот же исход"
        );
        assert!(flash.swap_requested(), "обмен помечен");
        assert_eq!(flash.magics_swap(), 1, "обмен помечен одним Finish");
    }

    /// Чужой `Finish` после `Ready` — `NoSession`: приёма у него не было.
    #[test]
    fn a_foreign_finish_after_ready_is_no_session() {
        let mut flash = flash(128);

        let replies = run(
            &mut flash,
            vec![start(16), chunk(0, &image(16)), finish(), by(1, finish())],
        );

        assert_eq!(replies.last(), Some(&Err(Rejection::NoSession)));
        assert!(flash.swap_requested());
    }

    #[test]
    fn maps_a_bad_length_to_length_without_erasing() {
        let mut flash = flash(128);

        let replies = run(&mut flash, vec![start(129)]);

        assert_eq!(replies, vec![Err(Rejection::Length)]);
        assert!(!flash.prepared());
    }

    /// Лишние байты — `Transfer`, и приём брошен: следующий кусок уже без
    /// приёма.
    #[test]
    fn excess_data_is_a_transfer_failure_that_ends_the_session() {
        let mut flash = flash(128);

        let replies = run(
            &mut flash,
            vec![
                start(16),
                chunk(0, &[0; 16]),
                chunk(16, &[0; 8]),
                chunk(24, &[0; 8]),
            ],
        );

        assert_eq!(
            &replies[2..],
            &[Err(Rejection::Transfer), Err(Rejection::NoSession)]
        );
        assert!(!flash.swap_requested(), "обмен не запрашивался");
    }

    /// `Finish` до последнего байта — `Transfer`, обмен не запрошен.
    #[test]
    fn an_early_finish_is_a_transfer_failure() {
        let mut flash = flash(128);

        let replies = run(&mut flash, vec![start(16), chunk(0, &image(8)), finish()]);

        assert_eq!(replies.last(), Some(&Err(Rejection::Transfer)));
        assert!(!flash.swap_requested());
    }

    #[test]
    fn maps_a_flash_failure_on_apply_to_device() {
        let mut flash = flash(128);
        flash.refuse_swap();

        let replies = run(&mut flash, vec![start(8), chunk(0, &image(8)), finish()]);

        assert_eq!(replies.last(), Some(&Err(Rejection::Device)));
    }

    /// Кусок и `Finish` без `Start` — `NoSession`, и узел работает дальше.
    #[test]
    fn a_chunk_or_finish_without_start_is_no_session() {
        let mut flash = flash(128);

        let replies = run(
            &mut flash,
            vec![
                chunk(0, &[0; 8]),
                finish(),
                start(8),
                chunk(0, &image(8)),
                finish(),
            ],
        );

        assert_eq!(
            replies,
            vec![
                Err(Rejection::NoSession),
                Err(Rejection::NoSession),
                Ok(Progress::Started),
                Ok(Progress::Received(8)),
                Ok(Progress::Ready),
            ]
        );
    }

    /// Повтор куска (ответ на него потерялся) подтверждается без записи, и
    /// образ собирается верно; кусок не встык — `Transfer`, приём брошен.
    #[test]
    fn a_repeated_chunk_is_acknowledged_and_a_gap_ends_the_session() {
        let mut flash = flash(128);
        let image = image(16);

        let replies = run(
            &mut flash,
            vec![
                start(16),
                chunk(0, &image[..8]),
                chunk(0, &image[..8]),
                chunk(8, &image[8..]),
                finish(),
                start(16),
                chunk(8, &image[..8]),
            ],
        );

        assert_eq!(
            replies,
            vec![
                Ok(Progress::Started),
                Ok(Progress::Received(8)),
                Ok(Progress::Received(8)),
                Ok(Progress::Received(16)),
                Ok(Progress::Ready),
                Err(Rejection::Busy),
                Err(Rejection::NoSession),
            ]
        );
        assert_eq!(&flash.memory()[..16], &image[..]);
    }

    /// Кусок не встык к принятым — `Transfer`, приём брошен.
    #[test]
    fn a_chunk_out_of_order_is_a_transfer_failure() {
        let mut flash = flash(128);

        let replies = run(
            &mut flash,
            vec![
                start(32),
                chunk(0, &[0; 8]),
                chunk(16, &[0; 8]),
                chunk(8, &[0; 8]),
            ],
        );

        assert_eq!(
            &replies[2..],
            &[Err(Rejection::Transfer), Err(Rejection::NoSession)]
        );
    }

    /// Хост замолчал дольше тайм-аута — приём брошен, кусок после паузы уже
    /// без приёма; молчание короче тайм-аута приём не прерывает.
    #[test]
    fn silence_longer_than_the_timeout_abandons_the_session() {
        let mut flash = flash(128);
        let almost = SESSION_TIMEOUT - Duration::from_millis(1);

        let replies = run(
            &mut flash,
            vec![
                start(32),
                Step::Wait(almost),
                chunk(0, &[0; 8]),
                Step::Wait(almost),
                chunk(8, &[0; 8]),
                Step::Wait(SESSION_TIMEOUT),
                chunk(16, &[0; 8]),
            ],
        );

        assert_eq!(
            replies,
            vec![
                Ok(Progress::Started),
                Ok(Progress::Received(8)),
                Ok(Progress::Received(16)),
                Err(Rejection::NoSession),
            ]
        );
    }

    /// Чужой `Start` посреди приёма — `Busy`, приём владельца не тронут.
    #[test]
    fn a_foreign_start_during_receiving_is_busy() {
        let mut flash = flash(128);
        let image = image(16);

        let replies = run(
            &mut flash,
            vec![start(16), by(1, start(8)), chunk(0, &image), finish()],
        );

        assert_eq!(
            replies,
            vec![
                Ok(Progress::Started),
                Err(Rejection::Busy),
                Ok(Progress::Received(16)),
                Ok(Progress::Ready),
            ]
        );
        assert_eq!(flash.prepares(), 1, "чужой Start раздел не готовил");
        assert_eq!(&flash.memory()[..16], &image[..]);
    }

    /// Чужой кусок с тем смещением, что ждёт владелец, — `Busy` и не
    /// записан; кусок владельца следом принят.
    #[test]
    fn a_foreign_chunk_is_busy_and_not_written() {
        let mut flash = flash(128);
        let image = image(16);

        let replies = run(
            &mut flash,
            vec![
                start(16),
                by(1, chunk(0, &[0xAA; 16])),
                chunk(0, &image),
                finish(),
            ],
        );

        assert_eq!(
            replies,
            vec![
                Ok(Progress::Started),
                Err(Rejection::Busy),
                Ok(Progress::Received(16)),
                Ok(Progress::Ready),
            ]
        );
        assert_eq!(&flash.memory()[..16], &image[..]);
    }

    /// Входы `CMD` и `ASK` — один автомат: приём, начатый через `ASK`,
    /// идёт через него, а команда клиента с `CMD` получает `Busy`.
    #[test]
    fn both_inputs_drive_one_session() {
        let mut flash = flash(128);
        let image = image(16);

        let replies = run(
            &mut flash,
            vec![
                asked(by(1, start(16))),
                chunk(0, &[0xAA; 16]),
                asked(by(1, chunk(0, &image))),
                asked(by(1, finish())),
            ],
        );

        assert_eq!(
            replies,
            vec![
                Ok(Progress::Started),
                Err(Rejection::Busy),
                Ok(Progress::Received(16)),
                Ok(Progress::Ready),
            ]
        );
        assert_eq!(&flash.memory()[..16], &image[..]);
    }

    /// Чужой `Finish` — `Busy`, приём продолжается.
    #[test]
    fn a_foreign_finish_is_busy() {
        let mut flash = flash(128);

        let replies = run(
            &mut flash,
            vec![start(8), by(1, finish()), chunk(0, &image(8)), finish()],
        );

        assert_eq!(
            replies,
            vec![
                Ok(Progress::Started),
                Err(Rejection::Busy),
                Ok(Progress::Received(8)),
                Ok(Progress::Ready),
            ]
        );
        assert_eq!(
            flash.magics_swap(),
            1,
            "обмен запрошен одним Finish владельца"
        );
    }

    /// Владелец повторил `Start` (ответ потерялся) — приём с нуля, не `Busy`.
    #[test]
    fn the_owner_may_restart_its_session() {
        let mut flash = flash(128);
        let image = image(16);

        let replies = run(
            &mut flash,
            vec![
                start(32),
                chunk(0, &[0xAA; 8]),
                start(16),
                chunk(0, &image),
                finish(),
            ],
        );

        assert_eq!(
            replies,
            vec![
                Ok(Progress::Started),
                Ok(Progress::Received(8)),
                Ok(Progress::Started),
                Ok(Progress::Received(16)),
                Ok(Progress::Ready),
            ]
        );
        assert_eq!(flash.prepares(), 2, "владелец начал заново");
    }

    /// Владелец замолчал — по сроку от его последней команды сессия свободна;
    /// чужой `Start` посреди срока его не продлевает.
    #[test]
    fn a_silent_owner_releases_the_session() {
        let mut flash = flash(128);
        let half = SESSION_TIMEOUT / 2;

        let replies = run(
            &mut flash,
            vec![
                start(16),
                Step::Wait(half),
                by(1, start(8)),
                Step::Wait(SESSION_TIMEOUT - half),
                by(1, start(8)),
            ],
        );

        assert_eq!(
            replies,
            vec![
                Ok(Progress::Started),
                Err(Rejection::Busy),
                Ok(Progress::Started),
            ]
        );
        assert_eq!(flash.prepares(), 2, "сессия клиента 1 — вторая подготовка");
    }

    /// Чужой `Start` после срока владельца, но раньше, чем узел заметил срок
    /// (команда выиграла опрос у `expired`), — не `Busy`: приём владельца
    /// брошен, начинается приём нового клиента.
    #[test]
    fn a_foreign_start_past_the_owners_deadline_takes_over() {
        let mut flash = flash(128);

        let replies = run(
            &mut flash,
            vec![start(16), Step::Skip(SESSION_TIMEOUT), by(1, start(8))],
        );

        assert_eq!(replies, vec![Ok(Progress::Started), Ok(Progress::Started)]);
        assert_eq!(flash.prepares(), 2, "сессия клиента 1 — вторая подготовка");
    }

    /// `Reboot` обслуживается от любого клиента, и посреди чужого приёма тоже.
    #[test]
    fn reboot_is_served_for_any_client() {
        let mut flash = flash(128);

        let replies = run(&mut flash, vec![start(16), by(1, send(Command::Reboot))]);

        assert_eq!(
            replies,
            vec![Ok(Progress::Started), Ok(Progress::Rebooting)]
        );
    }

    /// `Reboot`: ответ уходит, МК сбрасывается после паузы — не раньше.
    #[test]
    #[should_panic(expected = "МК сброшен")]
    fn reboot_replies_and_resets_after_the_grace_period() {
        let mut flash = flash(128);

        let replies = run(
            &mut flash,
            vec![
                send(Command::Reboot),
                Step::Wait(REBOOT_GRACE - Duration::from_millis(1)),
            ],
        );
        assert_eq!(replies, vec![Ok(Progress::Rebooting)], "ответ — до сброса");

        run(
            &mut flash,
            vec![send(Command::Reboot), Step::Wait(REBOOT_GRACE)],
        );
    }

    /// Без подписи в `Start` — отказ ДО стирания: стирание уничтожило бы
    /// образ, в который устройство откатывается, а применить всё равно нечем.
    #[test]
    fn refuses_an_update_without_a_signature_before_erasing() {
        let mut flash = flash(128);

        let replies = run(&mut flash, vec![unsigned_start(8), chunk(0, &[0; 8])]);

        assert_eq!(
            replies,
            vec![Err(Rejection::Signature), Err(Rejection::NoSession)]
        );
        assert!(!flash.prepared(), "раздел не должен быть стёрт");
    }

    /// Та же версия — откат, и код для хоста свой.
    #[test]
    fn maps_a_rollback_to_rollback() {
        let image = versioned_image(64, RUNNING_VERSION);
        let mut flash = flash(1024);

        let replies = run(&mut flash, update(&image));

        assert_eq!(replies.last(), Some(&Err(Rejection::Rollback)));
        assert!(flash.recorded().is_none(), "обмен не должен быть запрошен");
        assert!(!flash.swap_requested());
    }

    /// Образ короче хвоста (раскладка и версия) — `Length`, а не паника на
    /// вычитании; обмен не запрошен.
    #[test]
    fn maps_an_image_shorter_than_the_tail_to_length() {
        let mut flash = flash(1024);

        let replies = run(&mut flash, update(&[0; 7]));

        assert_eq!(replies.last(), Some(&Err(Rejection::Length)));
        assert!(flash.recorded().is_none());
        assert!(!flash.swap_requested());
    }

    /// Чужая раскладка — `Layout`, а не `Rollback`, хотя версия новее; обмен
    /// не запрошен, приём окончен.
    #[test]
    fn refuses_an_image_for_another_layout_before_marking_the_swap() {
        let mut image = image(64);
        image[56..60].copy_from_slice(&(!LAYOUT).to_le_bytes());
        let mut flash = flash(1024);
        let mut steps = update(&image);
        steps.push(chunk(0, &[0; 8]));

        let replies = run(&mut flash, steps);

        assert_eq!(
            replies[replies.len() - 2..],
            [Err(Rejection::Layout), Err(Rejection::NoSession)]
        );
        assert!(flash.recorded().is_none());
        assert!(
            !flash.swap_requested(),
            "обмен под чужую раскладку не помечается"
        );
    }

    /// Отказ флеша при записи для bootloader'а — `Device`, обмен не помечен.
    #[test]
    fn maps_a_flash_failure_on_the_signature_record_to_device() {
        let image = image(64);
        let mut flash = flash(1024);
        flash.refuse_record();

        let replies = run(&mut flash, update(&image));

        assert_eq!(replies.last(), Some(&Err(Rejection::Device)));
        assert!(!flash.swap_requested());
    }

    /// Шаг случайного сценария: что прислать и от какого клиента.
    fn arbitrary_step() -> impl proptest::strategy::Strategy<Value = Step> {
        use proptest::prelude::*;
        let command = prop_oneof![
            prop::sample::select(vec![8usize, 16, 64]).prop_map(start),
            (0u32..72, 1usize..20).prop_map(|(offset, len)| {
                let image = image(64);
                let from = (offset as usize).min(image.len() - 1);
                chunk(offset, &image[from..(from + len).min(image.len())])
            }),
            Just(finish()),
        ];
        prop_oneof![
            4 => (command, 0u8..2).prop_map(|(step, client)| by(client, step)),
            1 => Just(Step::Wait(SESSION_TIMEOUT + Duration::from_secs(1))),
        ]
    }

    proptest::proptest! {
        /// Какие бы команды ни пришли и в каком бы порядке — от двух клиентов, с
        /// перерывами дольше срока сессии, — узел не падает, отвечает на каждую
        /// ровно одним исходом и помечает обмен ровно тогда, когда ответил
        /// `Ready`.
        /// Сценарии выше проверяют по одной истории, здесь их тысячи.
        #[test]
        fn any_command_sequence_gets_one_reply_each_and_swaps_only_on_ready(
            steps in proptest::collection::vec(arbitrary_step(), 1..24)
        ) {
            let commands = steps.iter().filter(|step| !matches!(step, Step::Wait(_))).count();
            let mut flash = flash(1024);
            let replies = run(&mut flash, steps);
            proptest::prop_assert_eq!(replies.len(), commands);
            let ready = replies.iter().filter(|reply| **reply == Ok(Progress::Ready)).count();
            proptest::prop_assert_eq!(flash.swap_requested(), ready > 0);
        }
    }
}
