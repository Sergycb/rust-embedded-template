//! Узел `OTA` графа задач: принимает образ командами от узла протокола и
//! применяет его.
//!
//! Здесь, а не в `crates-cross/app`, по общему правилу проекта (см.
//! `domain::heartbeat`): что узел делает — логика, и проверяется она на хосте
//! на фейках портов.
//!
//! Образ приходит не по своему каналу: узел протокола (protobuf, Modbus,
//! CANopen — какой выбран при генерации) переводит свои сообщения в
//! [`OtaCommand`](ports::OtaCommand) и отдаёт их узлу через вход `CMD` —
//! `request:` графа, то есть запрос с ответом. На каждую команду узел отвечает
//! [`OtaReply`](ports::OtaReply) — её исходом, и протокол передаёт его хосту
//! как есть: ни опрашивать статус, ни угадывать, дошёл ли кусок, хосту не
//! нужно. Узел про протокол не знает, протокол — про флеш.
//!
//! Задача — автомат `#[fsm::typestate]` на контексте `OtaCtx`, который
//! эмитит фрагмент `OTA_FRAG`:
//!
//! * `Idle` — приёма нет; `Start` проверяет занятость и подпись и, если всё
//!   в порядке, стирает раздел под образ;
//! * `Receiving` — идёт приём: куски пишутся во флеш шагами
//!   [`Transfer`](crate::ota::download::Transfer), `Finish` применяет образ;
//!   хост молчит дольше [`SESSION_TIMEOUT`](crate::ota::SESSION_TIMEOUT) — приём
//!   брошен;
//! * `Ready` — образ применён, обмен разделов ждёт сброса; новый `Start`
//!   отвергается как `Busy`.
//!
//! `Reboot` принимается в любом состоянии: узел отвечает, даёт протоколу
//! [`REBOOT_GRACE`](crate::ota::REBOOT_GRACE) отправить ответ и сбрасывает МК портом
//! [`Reboot`](ports::Reboot). Сброс — команда хоста, а не следствие `Finish`:
//! так хост узнаёт исход образа до сброса, а не по факту перезагрузки.
//!
//! Применение различается по Cargo-варианту (`signed`), а Liquid в `domain`
//! запрещён, поэтому режим — данные входа узла: [`Mode::Plain`](crate::ota::Mode::Plain) или
//! [`Mode::Signed`](crate::ota::Mode::Signed), и выбирает его `crates-cross/app/src/graph.rs`.
//! Подробности — `docs/ota.md`.

/// Приём образа по кускам: сверка длины, стирание один раз, буферизация до
/// слова флеша — шаги над портом `FirmwareUpdate`, см. модуль.
pub mod download;

/// Решение bootloader'а при старте и проверка подписи нового образа — над
/// блокирующими разделами: у bootloader'а нет исполнителя. Функции над
/// `embassy-boot`, не над портами — см. модуль.
pub mod boot;

/// Применение подписанного обновления: порядок проверок, в котором каждый
/// отказ различим, и запись подписи для bootloader'а. Функция над портом
/// `FirmwareUpdate` — см. модуль.
pub mod update;

/// Версия прошивки в 32 битах и правило приёма обновления (защита от
/// отката). Образец границы: `bsp` достаёт четыре байта из флеша, `domain`
/// решает, что с ними делать, — см. модуль.
pub mod version;

// Типы входа `CMD` — под путём `domain`: фрагмент называет их полным путём, а
// compose-site (`app`) от `ports` не зависит.
pub use ports::{OtaCommand, OtaReply};

use defmt_or_log::{FormatOrDebug, info, warn};
use embassy_time::{Duration, Instant, Timer};
use ports::{
    Chunk, DownloadError, FirmwareUpdate, Progress, Reboot, Rejection, SIGNATURE_BYTES, UpdateError,
};
use supervisor::Served;
use supervisor::runtime::TaskExit;

use crate::ota::download::Transfer;

/// Сколько приём ждёт следующую команду, прежде чем его бросить.
///
/// Хост пропал посреди образа — раздел остаётся стёртым наполовину, но это
/// раздел обновления, не активный: ничего не сломано, следующий `Start`
/// начнёт заново. Без тайм-аута узел ждал бы недостающие куски вечно, а
/// `Chunk` от нового хоста лёг бы в середину чужого образа.
pub const SESSION_TIMEOUT: Duration = Duration::from_secs(30);

/// Сколько узел ждёт после ответа на `Reboot`, прежде чем сбросить МК.
///
/// Ответ уходит хосту через узел протокола, а тот работает, только когда узел
/// OTA уступит исполнитель: без паузы МК сбросился бы раньше, чем ответ
/// покинул устройство.
pub const REBOOT_GRACE: Duration = Duration::from_millis(100);

/// Подпись образа.
type Signature = [u8; SIGNATURE_BYTES];

/// Ёмкость входа `CMD`, байт: команда (кусок `ports::CHUNK_BYTES` со смещением и
/// длиной, тег варианта) и ответ. Одно число на граф (`request:` в `OTA_FRAG`) и
/// на `static` входа в проекте без графа (`crates-cross/app/src/main.rs`). В
/// `request:` фрагмента оно записано литералом — макрос не принимает путь, —
/// так что меняется в двух местах.
pub const CMD_BYTES: usize = 144;

const _: () = assert!(
    size_of::<OtaCommand>() <= CMD_BYTES && size_of::<OtaReply>() <= CMD_BYTES,
    "команда OTA не помещается во вход CMD: поднимите CMD_BYTES"
);

// Вход узла — `OtaWiring`: структуру эмитит сам фрагмент (`boot: …
// struct`). Строит её compose-site (`app`) из поля платы `Board::ota` —
// адаптера обновления (порт `FirmwareUpdate`; `bsp` про `domain` не знает) —
// и порта перезагрузки, подставляя их типы аргументами фрагмента:
//
// ```ignore
// fragments: [::domain::OTA_FRAG<bsp::ota::Updater, bsp::cpu::Reboot> as OTA
//                 = ::domain::ota::OtaWiring { flash: board.ota,
//                                           reboot: bsp::cpu::Reboot,
//                                           mode: ::domain::ota::Mode::Plain }];
// ```
//
// Тип объявлен здесь, а не назван графом, по правилу фрагментов
// `supervisor`: подсистема говорит, что ей нужно, приложение это
// конструирует — зависимость направлена от приложения к подсистеме и никогда
// обратно.

/// Чем режимы отличаются: проверка `Start` до стирания и применение.
///
/// Данные, а не трейт и не указатели на функции: применение асинхронное (порт
/// может стоять на внешней флеши), а указатель на `async fn` не сохранить; и
/// обоим режимам хватает одного порта [`FirmwareUpdate`] — запись о подписи
/// для bootloader'а это обычная запись в раздел.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    /// Без подписи: принятый образ применяется
    /// [`FirmwareUpdate::mark_updated`].
    Plain,
    /// С подписью: принятый образ применяется
    /// [`apply_signed`](crate::ota::update::apply_signed) — длина, занятость,
    /// версия, в этом порядке, и запись подписи для bootloader'а, который её и
    /// проверит перед обменом.
    ///
    /// Отличие до приёма одно: `Start` без подписи отвергается ДО стирания
    /// раздела — применить такой образ всё равно нечем, а стирание уничтожило
    /// бы образ, в который устройство откатывается.
    Signed {
        /// Версия этой прошивки (`FW_VERSION` приложения) — защита от отката.
        running_version: u32,
    },
}

impl Mode {
    /// Проверка `Start` ДО стирания раздела.
    fn check(self, signature: Option<&Signature>) -> Result<(), Rejection> {
        match self {
            Self::Plain => Ok(()),
            Self::Signed { .. } if signature.is_some() => Ok(()),
            Self::Signed { .. } => Err(Rejection::Signature),
        }
    }

    /// Применение принятого образа длиной `len`.
    async fn apply<F>(
        self,
        flash: &mut F,
        signature: Option<&Signature>,
        len: u32,
    ) -> Result<(), Rejection>
    where
        F: FirmwareUpdate,
        F::Error: FormatOrDebug,
    {
        match self {
            // Без подписи применение — просьба об обмене разделов; проверок у
            // неё нет намеренно (см. `FirmwareUpdate::mark_updated`).
            Self::Plain => flash.mark_updated().await.map_err(|err| {
                warn!("ota: пометить обмен не удалось: {:?}", &err);
                Rejection::Device
            }),
            Self::Signed { running_version } => {
                // `check` уже отказал бы без подписи; `ok_or` — чтобы не
                // паниковать, а не потому что отказ здесь достижим.
                let signature = signature.ok_or(Rejection::Signature)?;
                update::apply_signed(flash, running_version, signature, len)
                    .await
                    .map_err(|err| {
                        warn!("ota: применение отвергнуто: {:?}", &err);
                        update_rejection(&err)
                    })
            }
        }
    }
}

/// Узел `OTA`: отвечает на команды приёма образа, пока жив граф.
///
/// Не выходит никогда: каждая команда получает ответ — исход или код
/// отказа, — и узел ждёт следующую.
///
/// Без `watchdog:` намеренно — по тому же правилу, что у
/// [`heartbeat::task`](crate::heartbeat::task): отметка ставится после
/// реального прогресса, а команды может не быть всю жизнь платы. Аппаратный
/// сторож кормит тикер графа, а не какой-то узел.
pub async fn task<F, R>(mut ctx: OtaCtx<'_, F, R>) -> TaskExit
where
    F: FirmwareUpdate,
    F::Error: FormatOrDebug,
    R: Reboot,
{
    match ctx.run().await {}
}

#[fsm::typestate]
impl<F, R> OtaCtx<'_, F, R>
where
    F: FirmwareUpdate,
    F::Error: FormatOrDebug,
    R: Reboot,
{
    fsm::machine! {
        machine Ota mod edges {
            states {
                /// Приёма нет.
                Idle,
                /// Идёт приём образа; `until` — когда бросить его, если хост
                /// замолчал.
                Receiving {
                    transfer: Transfer,
                    signature: Option<Signature>,
                    until: Instant,
                },
                /// Образ применён, обмен разделов — на следующем сбросе.
                Ready,
            }
            start => begin -> Idle;
            listen {
                // Команда первой: срок — молчание хоста, и уже пришедшая
                // команда его не нарушила.
                *                       => cmd: self.cmd.serve(),
                Receiving { until, .. } => expired: fsm::deadline(*until),
            }
            transitions {
                *         + cmd((OtaCommand::Start { len, signature }, served)) => start      -> Receiving | Idle | Ready,
                Receiving + cmd((OtaCommand::Chunk(ref chunk), served))        => chunk      -> Receiving | Idle,
                Receiving + cmd((OtaCommand::Finish, served))                  => finish     -> Ready | Idle,
                Idle      + cmd((OtaCommand::Chunk(_) | OtaCommand::Finish, served)) => no_session -> self,
                Ready     + cmd((OtaCommand::Chunk(_) | OtaCommand::Finish, served)) => no_session -> self,
                *         + cmd((OtaCommand::Reboot, served))                  => reboot     -> self,
                Receiving + expired(())                                        => abandon    -> Idle,
            }
        }
    }

    /// Подтверждение образа, затем ожидание команд.
    ///
    /// Без подтверждения bootloader считает попытку неудачной и на следующем
    /// сбросе откатывает образ — обновление отработало бы ровно один запуск.
    /// Здесь, а не в `main` до исполнителя: порт асинхронный, и драйвер внешней
    /// флеши может ждать таймер или прерывание, а это требует исполнителя. На
    /// обычной загрузке вызов ничего не меняет (состояние и так `Boot`).
    ///
    /// Место — самое раннее из возможных, и это компромисс шаблона: он не
    /// знает, что для устройства значит «работает». Появится критерий
    /// (подняли связь, отработали цикл измерений) — переносите подтверждение
    /// туда; тогда откат застрахует и от образа, который стартует, но не
    /// работает.
    ///
    /// Ошибка — не повод ронять узел: устройство работает, просто следующий
    /// сброс вернёт прежний образ. Знать об этом важнее, чем упасть.
    ///
    /// Один раз за запуск (`confirmed` — слот): `STATE` после обмена и
    /// `STATE` с принятым, но ещё не применённым образом выглядят одинаково
    /// (`Swap`), и подтверждение после `mark_updated` молча сняло бы запрос на
    /// обмен, о котором хост уже услышал `Ready`. Не удалось — флаг не
    /// ставится, и узел пробует ещё раз на следующем `Start`.
    async fn begin(&mut self) -> Idle {
        info!("domain: узел OTA запущен");
        self.confirm().await;
        Idle
    }

    /// Подтверждение образа, если его ещё не было (см. `begin`).
    async fn confirm(&mut self) {
        if !*self.confirmed {
            match self.flash.mark_booted().await {
                Ok(()) => *self.confirmed = true,
                Err(err) => warn!("ota: не удалось подтвердить образ: {:?}", &err),
            }
        }
    }

    /// `Start`: проверки до стирания — занятость, затем режим (подпись), —
    /// и подготовка раздела. Незаконченный приём брошен.
    async fn start(
        &mut self,
        from: edges::StartFrom,
        len: u32,
        signature: Option<Signature>,
        served: Served<'_, OtaCommand, OtaReply>,
    ) -> edges::Start {
        info!("ota: Start: {} байт", len);
        // Образ, ждущий обмена, затирать нельзя: `Busy`, и он ждёт дальше. Узел
        // знает это сам, не спрашивая адаптер, — и не зовёт подтверждение,
        // которое сняло бы запрос на обмен (см. `begin`).
        let back: edges::Start = match from {
            edges::StartFrom::Ready(Ready) => {
                warn!("ota: Start отвергнут: образ ждёт обмена, нужен Reboot");
                served.respond(Err(Rejection::Busy));
                return Ready.into();
            }
            edges::StartFrom::Receiving(_) => {
                warn!("ota: незаконченный приём брошен новым Start");
                Idle.into()
            }
            edges::StartFrom::Idle(Idle) => Idle.into(),
        };
        // Не удалось подтвердить при старте — ещё раз перед новым образом, а
        // не чаще: неудавшийся `mark_booted` может стоить стирания `STATE`.
        self.confirm().await;
        // Занятость — до всего: адаптер и так откажет в `prepare`, но тогда
        // хост услышал бы `Device` вместо «сначала перезагрузи».
        let screened = match self.flash.is_busy().await {
            Ok(false) => self.mode.check(signature.as_ref()),
            Ok(true) => Err(Rejection::Busy),
            Err(err) => {
                warn!("ota: флеш не ответил о занятости: {:?}", &err);
                Err(Rejection::Device)
            }
        };
        if let Err(rejection) = screened {
            warn!("ota: Start отвергнут: {:?}", rejection);
            served.respond(Err(rejection));
            return back;
        }
        match Transfer::start(&mut *self.flash, len).await {
            Ok(transfer) => {
                served.respond(Ok(Progress::Started));
                Receiving {
                    transfer,
                    signature,
                    until: Instant::now() + SESSION_TIMEOUT,
                }
                .into()
            }
            Err(err) => {
                warn!("ota: Start отвергнут: {:?}", &err);
                served.respond(Err(download_rejection(&err)));
                Idle.into()
            }
        }
    }

    /// `Chunk`: кусок во флеш. Отказ — приём брошен, хост начинает заново.
    async fn chunk(
        &mut self,
        mut s: Receiving,
        chunk: &Chunk,
        served: Served<'_, OtaCommand, OtaReply>,
    ) -> edges::Chunk {
        match s
            .transfer
            .chunk(&mut *self.flash, chunk.offset(), chunk.as_bytes())
            .await
        {
            Ok(received) => {
                served.respond(Ok(Progress::Received(received)));
                s.until = Instant::now() + SESSION_TIMEOUT;
                s.into()
            }
            Err(err) => {
                warn!("ota: кусок отвергнут, приём брошен: {:?}", &err);
                served.respond(Err(download_rejection(&err)));
                Idle.into()
            }
        }
    }

    /// `Finish`: сверка длины, затем применение через режим (plain или
    /// signed).
    async fn finish(
        &mut self,
        s: Receiving,
        served: Served<'_, OtaCommand, OtaReply>,
    ) -> edges::Finish {
        let outcome = match s.transfer.finish(&mut *self.flash).await {
            Ok(len) => {
                self.mode
                    .apply(&mut *self.flash, s.signature.as_ref(), len)
                    .await
            }
            Err(err) => {
                warn!("ota: приём не сошёлся: {:?}", &err);
                Err(download_rejection(&err))
            }
        };
        match outcome {
            Ok(()) => {
                // Обмен запрошен (`STATE = Swap`): подтверждение текущего
                // образа теперь сняло бы запрос — и раз не вышло при старте,
                // больше его не пробуем (см. `begin`).
                *self.confirmed = true;
                info!("ota: образ принят, обмен разделов на следующем сбросе");
                served.respond(Ok(Progress::Ready));
                Ready.into()
            }
            Err(rejection) => {
                warn!("ota: образ отвергнут: {:?}", rejection);
                served.respond(Err(rejection));
                Idle.into()
            }
        }
    }

    /// `Chunk` или `Finish` без приёма: отказ, состояние то же.
    #[expect(clippy::unused_self, reason = "действие автомата")]
    fn no_session(&mut self, served: Served<'_, OtaCommand, OtaReply>) {
        warn!("ota: кусок или Finish без Start");
        served.respond(Err(Rejection::NoSession));
    }

    /// `Reboot`: ответ, пауза на его отправку, сброс.
    async fn reboot(&mut self, served: Served<'_, OtaCommand, OtaReply>) {
        info!("ota: перезагрузка по команде хоста");
        served.respond(Ok(Progress::Rebooting));
        Timer::after(REBOOT_GRACE).await;
        self.reboot.reboot()
    }

    /// Хост замолчал посреди образа: приём брошен.
    #[expect(clippy::unused_self, reason = "действие автомата")]
    fn abandon(&mut self, _: Receiving) -> Idle {
        warn!("ota: хост молчит дольше тайм-аута — приём брошен");
        Idle
    }
}

/// Код отказа для хоста по ошибке приёма.
fn download_rejection<F>(err: &DownloadError<F>) -> Rejection {
    match err {
        DownloadError::Empty | DownloadError::TooLong { .. } => Rejection::Length,
        DownloadError::TooMuchData { .. }
        | DownloadError::Incomplete { .. }
        | DownloadError::OutOfOrder { .. } => Rejection::Transfer,
        // `UnusableGranularity`, `Flash` и незнакомые варианты (перечисление
        // `non_exhaustive`) сообщаются как отказ устройства, а не прячутся.
        _ => Rejection::Device,
    }
}

/// Код отказа для хоста по ошибке применения с подписью.
fn update_rejection<E>(err: &UpdateError<E>) -> Rejection {
    match err {
        UpdateError::TooLong { .. } | UpdateError::Truncated { .. } => Rejection::Length,
        UpdateError::Busy => Rejection::Busy,
        UpdateError::Rollback { .. } => Rejection::Rollback,
        // `Flash` и незнакомые варианты (перечисление `non_exhaustive`).
        _ => Rejection::Device,
    }
}

// Узел объявлен здесь же, где лежит его задача, — как `HEARTBEAT_FRAG` в
// `domain::heartbeat`; compose-site (`crates-cross/app/src/graph.rs`) только
// перечисляет фрагмент, называет узел (`as OTA`) и кормит его входом. Фрагмент
// один на оба Cargo-варианта `signed`: режим применения — данные входа
// (`OtaWiring::mode`), а не второй фрагмент. Какой режим подставить — решает
// `graph.rs`.
//
// `boot: inputs: …` — собственная привязка фрагмента: compose-site пишет
// `fragments: [::domain::OTA_FRAG<…> as OTA = ::domain::ota::OtaWiring { … }]`,
// а `spawn_all` связывает её первой строкой пролога, и инициализаторы слотов
// ниже читают её поля. `CONFIRMED` — без входа: подтверждён ли уже образ в
// этом запуске (см. действие `begin`).
//
// `request: [CMD: …]` — вход команд: запрос с ответом (`sync-request`), а не
// очередь, потому что протокол отвечает хосту исходом именно своей команды.
// Узел протокола получает клиента аксессором графа `ota_cmd_requester()`.
// Ёмкость — `CMD_BYTES` (литералом: макрос другого не принимает), её же берёт
// `static` входа в проекте без графа; менять оба места вместе.
//
// `<F, R>` — параметры фрагмента: адаптер обновления и порт перезагрузки
// платы `domain` не видит. `local` на `FLASH`: разделы платы — `!Send`
// (`adapters::ota::Updater` над ними держит `&'static Mutex<NoopRawMutex, …>`),
// а обычный слот — `static`, которому нужен `Send`. `local` меняет слот на
// `LocalResourceSlot` и снимает это требование, взамен обязывая держать
// значение на одном исполнителе — том, что спавнит граф (то же допущение, что
// `NoopRawMutex` у `FlashMutex`). Compose-site платит фичей
// `supervisor/local-resources` и `unsafe`-блоком на каждый `local`-`static`.
//
// Без `watchdog:` намеренно — см. `task`.
supervisor::supervisor_fragment! {
    name: OTA_FRAG<F, R>;
    boot: inputs: struct $crate::ota::OtaWiring;

    node deps: [], restart: Never,
        resources: [
            /// Адаптер обновления поверх разделов флеша (`FirmwareUpdate`).
            FLASH: local F = inputs.flash,
            /// Перезагрузка МК (`Reboot`) — для команды `Reboot`.
            REBOOT: R = inputs.reboot,
            /// Режим применения: `Mode::Plain` или `Mode::Signed`.
            MODE: $crate::ota::Mode = inputs.mode,
            CONFIRMED: bool = false
        ],
        // Литерал, а не `CMD_BYTES`: макрос принимает здесь только число. Равен ему.
        request: [CMD: $crate::ota::OtaCommand -> $crate::ota::OtaReply; 144],
        ctx: $crate::ota::OtaCtx,
        task: $crate::ota::task;
}

#[cfg(test)]
pub(crate) mod tests {
    use core::pin::{Pin, pin};
    use core::task::{Context, Poll, Waker};

    use embassy_time::{Duration, MockDriver};
    use ports::{Chunk, OtaCommand, OtaReply, Progress, Reboot, Rejection};
    // {%- if protocol != "none" %}
    use supervisor::Requester;
    use supervisor::runtime::TaskExit;
    // {%- endif %}

    use super::{Mode, REBOOT_GRACE, SESSION_TIMEOUT, task};
    use crate::ota::fakes::{FakeFlash, SIGNATURE};
    use crate::ota::update::VERSION_BYTES;
    use crate::ota::version::pack;

    /// Сброс МК в тесте — паника с узнаваемым текстом: `reboot` не
    /// возвращается, а `#[should_panic]` ловит именно её.
    struct FakeReboot;

    impl Reboot for FakeReboot {
        fn reboot(&mut self) -> ! {
            panic!("МК сброшен")
        }
    }

    // {%- if protocol != "none" %}
    /// Узел OTA, который тест крутит сам: `poll` по очереди с запросом.
    pub(crate) type Node = Pin<Box<dyn Future<Output = TaskExit>>>;

    /// Узел OTA на фейке флеша и его клиент — для тестов обработчиков
    /// протоколов (`host::service`). Стенд, флеш и слоты живут до конца
    /// процесса, как граф в прошивке: клиент — `'static`, как у
    /// `ota_cmd_requester()`.
    pub(crate) fn live() -> (Requester<'static, OtaCommand, OtaReply>, Node) {
        let stand = Box::leak(Box::new(
            supervisor::stand!(OTA_FRAG<FakeFlash, FakeReboot>),
        ));
        let node = task(stand.ctx(
            Box::leak(Box::new(FakeFlash::new(1024, 8))),
            Box::leak(Box::new(FakeReboot)),
            Box::leak(Box::new(Mode::Plain)),
            Box::leak(Box::new(false)),
        ));
        (stand.cmd.requester(), Box::pin(node))
    }
    // {%- endif %}

    /// Шаг сценария: команда узлу или пауза.
    enum Step {
        Send(OtaCommand),
        Wait(Duration),
    }

    fn poll<T>(fut: Pin<&mut impl Future<Output = T>>) -> Poll<T> {
        fut.poll(&mut Context::from_waker(Waker::noop()))
    }

    /// Гоняет узел по сценарию и возвращает ответы на команды по порядку.
    /// `confirmed` — слот снаружи, как у второго прогона узла в том же
    /// запуске прошивки.
    fn run_on(
        flash: &mut FakeFlash,
        mut mode: Mode,
        confirmed: &mut bool,
        steps: Vec<Step>,
    ) -> Vec<OtaReply> {
        MockDriver::get().reset();
        let stand = supervisor::stand!(OTA_FRAG<FakeFlash, FakeReboot>);
        let mut reboot = FakeReboot;
        let requester = stand.cmd.requester();
        let mut node = pin!(task(stand.ctx(flash, &mut reboot, &mut mode, confirmed)));
        assert!(poll(node.as_mut()).is_pending(), "узел ждёт команду");

        let mut replies = Vec::new();
        for step in steps {
            match step {
                Step::Send(cmd) => {
                    let mut request = pin!(requester.request(cmd));
                    let reply = (0..16)
                        .find_map(|_| match poll(request.as_mut()) {
                            Poll::Ready(reply) => Some(reply.expect("узел ответил")),
                            Poll::Pending => {
                                assert!(poll(node.as_mut()).is_pending(), "узел не выходит");
                                None
                            }
                        })
                        .expect("узел ответил за конечное число шагов");
                    replies.push(reply);
                }
                Step::Wait(pause) => {
                    MockDriver::get().advance(pause);
                    assert!(poll(node.as_mut()).is_pending(), "узел не выходит");
                }
            }
        }
        replies
    }

    fn run(flash: &mut FakeFlash, mode: Mode, steps: Vec<Step>) -> Vec<OtaReply> {
        run_on(flash, mode, &mut false, steps)
    }

    #[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
    fn start(len: usize) -> Step {
        Step::Send(OtaCommand::Start {
            len: len as u32,
            signature: None,
        })
    }

    #[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
    fn signed_start(len: usize) -> Step {
        Step::Send(OtaCommand::Start {
            len: len as u32,
            signature: Some(SIGNATURE),
        })
    }

    fn chunk(offset: u32, data: &[u8]) -> Step {
        Step::Send(OtaCommand::Chunk(
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
        Step::Send(OtaCommand::Finish)
    }

    #[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
    fn image(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 3) as u8).collect()
    }

    /// Счастливый путь: раздел подготовлен один раз, каждый кусок
    /// подтверждён принятой длиной, обмен запрошен, хост услышал `Ready`.
    #[test]
    fn applies_an_unsigned_image_and_replies_to_every_command() {
        let image = image(64);
        let mut flash = FakeFlash::new(1024, 8);
        let mut steps = vec![start(64)];
        steps.extend(chunks(&image));
        steps.push(finish());

        let replies = run(&mut flash, Mode::Plain, steps);

        assert_eq!(flash.prepared, Some(64));
        assert_eq!(&flash.memory[..64], &image[..]);
        assert_eq!(flash.updated, 1, "обмен разделов запрошен один раз");
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
        let mut flash = FakeFlash::new(64, 8);

        run(&mut flash, Mode::Plain, vec![]);

        assert_eq!(flash.booted, 1);
        assert_eq!(flash.prepared, None, "раздел не тронут");
    }

    /// Рестарт узла после принятого образа не подтверждает снова: иначе
    /// запрос на обмен, о котором хост услышал `Ready`, был бы молча снят.
    #[test]
    fn a_second_run_does_not_confirm_again() {
        let mut confirmed = false;
        let mut flash = FakeFlash::new(64, 8);

        run_on(
            &mut flash,
            Mode::Plain,
            &mut confirmed,
            vec![start(16), chunk(0, &image(16)), finish()],
        );
        assert_eq!((flash.booted, flash.updated), (1, 1));

        run_on(&mut flash, Mode::Plain, &mut confirmed, vec![]);
        assert_eq!(flash.booted, 1, "второй прогон не подтверждает");
    }

    /// Подтверждение не удалось, образов не было — узел работает дальше и
    /// пробует снова на следующем прогоне.
    #[test]
    fn a_failed_confirmation_is_retried_on_the_next_run() {
        let mut confirmed = false;
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_booted = Err("флеш не отвечает");

        run_on(&mut flash, Mode::Plain, &mut confirmed, vec![]);
        assert!(!confirmed);

        flash.mark_booted = Ok(());
        run_on(&mut flash, Mode::Plain, &mut confirmed, vec![]);
        assert!(confirmed);
    }

    /// Не удалось подтвердить при старте — узел пробует ещё раз на каждом
    /// `Start`, и только на нём: повтор стоит стирания `STATE`.
    #[test]
    fn a_failed_confirmation_is_retried_on_the_next_start() {
        let mut confirmed = false;
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_booted = Err("флеш не отвечает");

        // Образ длиннее раздела — отвергнут, но подтверждение пробовалось.
        run_on(
            &mut flash,
            Mode::Plain,
            &mut confirmed,
            vec![start(1000), chunk(0, &[0; 8])],
        );

        assert_eq!(
            flash.confirm_attempts, 2,
            "при старте и на Start, на куске — нет"
        );
        assert!(!confirmed);
    }

    /// Отказ подтверждения не мешает принять образ.
    #[test]
    fn a_failed_confirmation_does_not_block_an_update() {
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_booted = Err("флеш не отвечает");

        let replies = run(
            &mut flash,
            Mode::Plain,
            vec![start(8), chunk(0, &[0; 8]), finish()],
        );

        assert_eq!(replies.last(), Some(&Ok(Progress::Ready)), "образ принят");
    }

    /// Принятый образ отменяет повтор подтверждения: после `mark_updated`
    /// `mark_booted` снял бы запрос на обмен, о котором хост уже услышал
    /// `Ready`.
    #[test]
    fn an_accepted_image_is_never_followed_by_a_confirmation() {
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_booted = Err("флеш не отвечает");

        run(
            &mut flash,
            Mode::Plain,
            vec![start(16), chunk(0, &image(16)), finish(), start(16)],
        );

        assert_eq!(flash.updated, 1, "обмен запрошен");
        assert_eq!(
            flash.confirm_attempts, 2,
            "при старте и на первом Start, на втором — нет"
        );
    }

    /// Занятость — до стирания: адаптер и так отказал бы в `prepare`, но
    /// хост услышал бы `Device` вместо «перезагрузи».
    #[test]
    fn replies_busy_before_touching_the_partition() {
        let mut flash = FakeFlash::new(64, 8);
        flash.busy = true;

        let replies = run(&mut flash, Mode::Plain, vec![start(8)]);

        assert_eq!(replies, vec![Err(Rejection::Busy)]);
        assert_eq!(flash.prepared, None, "раздел не должен быть стёрт");
    }

    /// После `Ready` новый образ — `Busy`, а не стирание раздела с образом,
    /// ждущим обмена; узел по-прежнему ждёт `Reboot`.
    #[test]
    fn a_start_after_ready_is_busy_and_keeps_the_pending_image() {
        let mut flash = FakeFlash::new(64, 8);
        let image = image(16);

        let replies = run(
            &mut flash,
            Mode::Plain,
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
        assert_eq!(&flash.memory[..16], &image[..], "образ, ждущий обмена, цел");
    }

    #[test]
    fn maps_a_bad_length_to_length_without_erasing() {
        let mut flash = FakeFlash::new(64, 8);

        let replies = run(&mut flash, Mode::Plain, vec![start(65)]);

        assert_eq!(replies, vec![Err(Rejection::Length)]);
        assert_eq!(flash.prepared, None);
    }

    /// Лишние байты — `Transfer`, и приём брошен: следующий кусок уже без
    /// приёма.
    #[test]
    fn excess_data_is_a_transfer_failure_that_ends_the_session() {
        let mut flash = FakeFlash::new(64, 8);

        let replies = run(
            &mut flash,
            Mode::Plain,
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
        assert_eq!(flash.updated, 0, "обмен не запрашивался");
    }

    /// `Finish` до последнего байта — `Transfer`, обмен не запрошен.
    #[test]
    fn an_early_finish_is_a_transfer_failure() {
        let mut flash = FakeFlash::new(64, 8);

        let replies = run(
            &mut flash,
            Mode::Plain,
            vec![start(16), chunk(0, &[0; 8]), finish()],
        );

        assert_eq!(replies.last(), Some(&Err(Rejection::Transfer)));
        assert_eq!(flash.updated, 0);
    }

    /// Негодная гранулярность — вина устройства, не хоста, и до стирания.
    #[test]
    fn maps_an_unusable_granularity_to_device_without_erasing() {
        let mut flash = FakeFlash::new(64, 8);
        flash.word = 0;

        let replies = run(&mut flash, Mode::Plain, vec![start(8)]);

        assert_eq!(replies, vec![Err(Rejection::Device)]);
        assert_eq!(flash.prepared, None);
    }

    #[test]
    fn maps_a_flash_failure_on_apply_to_device() {
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_updated = Err("флеш отказал");

        let replies = run(
            &mut flash,
            Mode::Plain,
            vec![start(8), chunk(0, &[0; 8]), finish()],
        );

        assert_eq!(replies.last(), Some(&Err(Rejection::Device)));
    }

    /// Кусок и `Finish` без `Start` — `NoSession`, и узел работает дальше.
    #[test]
    fn a_chunk_or_finish_without_start_is_no_session() {
        let mut flash = FakeFlash::new(64, 8);

        let replies = run(
            &mut flash,
            Mode::Plain,
            vec![
                chunk(0, &[0; 8]),
                finish(),
                start(8),
                chunk(0, &[0; 8]),
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
        let mut flash = FakeFlash::new(64, 8);
        let image = image(16);

        let replies = run(
            &mut flash,
            Mode::Plain,
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
        assert_eq!(&flash.memory[..16], &image[..]);
    }

    /// Кусок не встык к принятым — `Transfer`, приём брошен.
    #[test]
    fn a_chunk_out_of_order_is_a_transfer_failure() {
        let mut flash = FakeFlash::new(64, 8);

        let replies = run(
            &mut flash,
            Mode::Plain,
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

    /// Новый `Start` посреди приёма начинает образ заново.
    #[test]
    fn a_start_while_receiving_starts_over() {
        let mut flash = FakeFlash::new(64, 8);
        let image = image(16);

        let replies = run(
            &mut flash,
            Mode::Plain,
            vec![
                start(32),
                chunk(0, &[0xAA; 8]),
                start(16),
                chunk(0, &image),
                finish(),
            ],
        );

        assert_eq!(replies.last(), Some(&Ok(Progress::Ready)));
        assert_eq!(&flash.memory[..16], &image[..]);
    }

    /// Хост замолчал дольше тайм-аута — приём брошен, кусок после паузы уже
    /// без приёма; молчание короче тайм-аута приём не прерывает.
    #[test]
    fn silence_longer_than_the_timeout_abandons_the_session() {
        let mut flash = FakeFlash::new(64, 8);
        let almost = SESSION_TIMEOUT - Duration::from_millis(1);

        let replies = run(
            &mut flash,
            Mode::Plain,
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

    /// `Reboot`: ответ уходит, МК сбрасывается после паузы — не раньше.
    #[test]
    #[should_panic(expected = "МК сброшен")]
    fn reboot_replies_and_resets_after_the_grace_period() {
        let mut flash = FakeFlash::new(64, 8);

        let replies = run(
            &mut flash,
            Mode::Plain,
            vec![
                Step::Send(OtaCommand::Reboot),
                Step::Wait(REBOOT_GRACE - Duration::from_millis(1)),
            ],
        );
        assert_eq!(replies, vec![Ok(Progress::Rebooting)], "ответ — до сброса");

        run(
            &mut flash,
            Mode::Plain,
            vec![Step::Send(OtaCommand::Reboot), Step::Wait(REBOOT_GRACE)],
        );
    }

    /// С подписью; версия «этой прошивки» — 1.2.3.
    fn signed() -> Mode {
        Mode::Signed {
            running_version: pack(1, 2, 3),
        }
    }

    /// Образ с версией в хвосте — как его собирает `cargo xtask build`.
    fn versioned_image(len: usize, version: u32) -> Vec<u8> {
        let mut image = image(len);
        image[len - VERSION_BYTES as usize..].copy_from_slice(&version.to_le_bytes());
        image
    }

    fn signed_update(image: &[u8]) -> Vec<Step> {
        let mut steps = vec![signed_start(image.len())];
        steps.extend(chunks(image));
        steps.push(finish());
        steps
    }

    /// Счастливый путь с подписью: подпись из `Start` и длина доехали до
    /// адаптера записью для bootloader'а, и только после неё помечен обмен.
    #[test]
    fn applies_a_signed_image_with_the_signature_from_start() {
        let image = versioned_image(64, pack(1, 2, 4));
        let mut flash = FakeFlash::new(1024, 8);

        let replies = run(&mut flash, signed(), signed_update(&image));

        assert_eq!(&flash.memory[..64], &image[..]);
        assert_eq!(flash.recorded, vec![(SIGNATURE, 64)]);
        assert_eq!(flash.updated, 1);
        assert_eq!(replies.last(), Some(&Ok(Progress::Ready)));
    }

    /// Без подписи в `Start` — отказ ДО стирания: стирание уничтожило бы
    /// образ, в который устройство откатывается, а применить всё равно нечем.
    #[test]
    fn refuses_a_signed_update_without_a_signature_before_erasing() {
        let mut flash = FakeFlash::new(64, 8);

        let replies = run(&mut flash, signed(), vec![start(8), chunk(0, &[0; 8])]);

        assert_eq!(
            replies,
            vec![Err(Rejection::Signature), Err(Rejection::NoSession)]
        );
        assert_eq!(flash.prepared, None, "раздел не должен быть стёрт");
    }

    /// Та же версия — откат, и код для хоста свой.
    #[test]
    fn maps_a_rollback_to_rollback() {
        let image = versioned_image(64, pack(1, 2, 3));
        let mut flash = FakeFlash::new(1024, 8);

        let replies = run(&mut flash, signed(), signed_update(&image));

        assert_eq!(replies.last(), Some(&Err(Rejection::Rollback)));
        assert!(flash.recorded.is_empty(), "обмен не должен быть запрошен");
    }

    /// Образ короче хвоста с версией — `Length`, а не паника на вычитании.
    #[test]
    fn maps_a_signed_image_shorter_than_the_version_tail_to_length() {
        let mut flash = FakeFlash::new(1024, 8);

        let replies = run(&mut flash, signed(), signed_update(&[0; 3]));

        assert_eq!(replies.last(), Some(&Err(Rejection::Length)));
        assert!(flash.recorded.is_empty());
    }

    /// Длина больше вместимости — `Length` ещё на `Start`, до стирания.
    #[test]
    fn maps_a_too_long_signed_image_to_length_without_erasing() {
        let mut flash = FakeFlash::new(64, 8);

        let replies = run(&mut flash, signed(), vec![signed_start(65)]);

        assert_eq!(replies, vec![Err(Rejection::Length)]);
        assert_eq!(flash.prepared, None);
    }

    /// Отказ флеша при записи для bootloader'а — `Device`, обмен не помечен.
    #[test]
    fn maps_a_flash_failure_on_the_signature_record_to_device() {
        let image = versioned_image(64, pack(1, 2, 4));
        let mut flash = FakeFlash::new(1024, 8);
        flash.record = Err("флеш отказал");

        let replies = run(&mut flash, signed(), signed_update(&image));

        assert_eq!(replies.last(), Some(&Err(Rejection::Device)));
        assert_eq!(flash.updated, 0);
    }
}

#[cfg(test)]
mod fakes {
    //! Фейки портов обновления для host-тестов.
    //!
    //! Один фейк на порт, общий для `download`, `update` и `ota`: сценарии у
    //! модулей разные, а придирки фейка — те же, что у настоящего адаптера
    //! (кратность записи слову, запись только после `prepare`), и держать их в
    //! трёх копиях значило бы проверять три разных флеша.
    //!
    //! Ни один фейк не ждёт по-настоящему — их future готовы с первого `poll`, —
    //! поэтому тестам хватает однопоточного `#[tokio::test]`.

    use ports::{FirmwareUpdate, PendingImage};

    use crate::ota::update::VERSION_BYTES;

    /// Подпись, которую тесты кладут в `Start`; фейк её не проверяет, а
    /// записывает — важно, что до него доехала именно она.
    pub(super) const SIGNATURE: [u8; 64] = [9; 64];

    /// Флеш в памяти, придирчивый ровно там же, где настоящий, плюс запасная
    /// страница за `capacity` — туда применение с подписью кладёт запись для
    /// bootloader'а.
    ///
    /// Главное здесь — проверки кратности: без них тест не отличил бы
    /// работающую буферизацию от её отсутствия, а на плате разница вылезла бы
    /// отказом записи (или, на F2/F4/F7, испорченным образом).
    pub(super) struct FakeFlash {
        pub(super) memory: Vec<u8>,
        /// То, что отдаёт `capacity()`. По умолчанию — длина `memory`; тесты
        /// применения ставят меньше, чтобы разыграть «в приёмник влез, в
        /// активный раздел — нет».
        pub(super) capacity: u32,
        pub(super) word: u32,
        pub(super) busy: bool,
        pub(super) prepared: Option<u32>,
        pub(super) writes: Vec<(u32, usize)>,
        /// Что ответить на запись в запасную страницу (запись для
        /// bootloader'а); `Err` разыгрывает отказ флеша.
        pub(super) record: Result<(), &'static str>,
        /// Что оставлено bootloader'у в запасной странице: подпись и длина.
        pub(super) recorded: Vec<([u8; 64], u32)>,
        /// Что ответить на `mark_updated`; `Err` разыгрывает отказ флеша.
        pub(super) mark_updated: Result<(), &'static str>,
        /// Сколько раз просили обмен разделов.
        pub(super) updated: usize,
        /// Что ответить на `mark_booted`; `Err` разыгрывает отказ флеша.
        pub(super) mark_booted: Result<(), &'static str>,
        /// Сколько раз образ подтвердили успешно.
        pub(super) booted: usize,
        /// Сколько раз звали `mark_booted` — удачно или нет.
        pub(super) confirm_attempts: usize,
    }

    impl FakeFlash {
        /// Запасная страница за `capacity` — под запись для bootloader'а.
        const SPARE: usize = 128;

        pub(super) fn new(capacity: usize, word: u32) -> Self {
            Self {
                memory: vec![0xFF; capacity + Self::SPARE],
                capacity: u32::try_from(capacity).unwrap(),
                word,
                busy: false,
                prepared: None,
                writes: Vec::new(),
                record: Ok(()),
                recorded: Vec::new(),
                mark_updated: Ok(()),
                updated: 0,
                mark_booted: Ok(()),
                booted: 0,
                confirm_attempts: 0,
            }
        }

        /// Раздел, в котором уже лежит образ длиной `len` с версией `incoming` в
        /// хвосте, и вместимость меньше раздела — как у настоящей схемы с обменом.
        pub(super) fn with_image(len: u32, incoming: u32) -> Self {
            let mut flash = Self::new(256, 8);
            flash.capacity = 128;
            flash.memory[(len - VERSION_BYTES) as usize..len as usize]
                .copy_from_slice(&incoming.to_le_bytes());
            flash
        }
    }

    impl FakeFlash {
        fn do_prepare(&mut self, len: u32) -> Result<(), &'static str> {
            if len == 0 || len > self.capacity {
                return Err("негодная длина");
            }
            self.prepared = Some(len);
            self.memory.fill(0xFF);
            Ok(())
        }

        fn do_write(&mut self, offset: u32, data: &[u8]) -> Result<(), &'static str> {
            if offset >= self.capacity {
                // Запасная страница: запись для bootloader'а — ровно там, где её
                // ищет `domain::ota::boot::screen_update`, и целыми словами.
                if offset != self.capacity {
                    return Err("запись для bootloader'а не по смещению capacity");
                }
                if !u32::try_from(data.len()).is_ok_and(|v| v.is_multiple_of(self.word)) {
                    return Err("длина записи не кратна слову");
                }
                self.record?;
                let bytes: &[u8; PendingImage::LEN] =
                    data.try_into().ok().ok_or("запись не той длины")?;
                let record = PendingImage::from_bytes(bytes).ok_or("не запись о подписи")?;
                self.recorded.push((record.signature, record.len));
                return Ok(());
            }
            if self.prepared.is_none() {
                return Err("запись без подготовки раздела");
            }
            if !u32::try_from(data.len()).is_ok_and(|v| v.is_multiple_of(self.word)) {
                return Err("длина записи не кратна слову");
            }
            if !offset.is_multiple_of(self.word) {
                return Err("смещение не кратно слову");
            }
            let start = offset as usize;
            self.memory[start..start + data.len()].copy_from_slice(data);
            self.writes.push((offset, data.len()));
            Ok(())
        }

        fn do_mark_updated(&mut self) -> Result<(), &'static str> {
            self.mark_updated?;
            self.updated += 1;
            Ok(())
        }
    }

    impl FirmwareUpdate for FakeFlash {
        type Error = &'static str;

        fn write_granularity(&mut self) -> u32 {
            self.word
        }

        fn capacity(&mut self) -> Result<u32, Self::Error> {
            Ok(self.capacity)
        }

        fn is_busy(&mut self) -> impl Future<Output = Result<bool, Self::Error>> {
            std::future::ready(Ok(self.busy))
        }

        fn prepare(&mut self, len: u32) -> impl Future<Output = Result<(), Self::Error>> {
            std::future::ready(self.do_prepare(len))
        }

        fn write(
            &mut self,
            offset: u32,
            data: &[u8],
        ) -> impl Future<Output = Result<(), Self::Error>> {
            std::future::ready(self.do_write(offset, data))
        }

        fn read(
            &mut self,
            offset: u32,
            buf: &mut [u8],
        ) -> impl Future<Output = Result<(), Self::Error>> {
            let start = offset as usize;
            buf.copy_from_slice(&self.memory[start..start + buf.len()]);
            std::future::ready(Ok(()))
        }

        fn mark_booted(&mut self) -> impl Future<Output = Result<(), Self::Error>> {
            self.confirm_attempts += 1;
            if self.mark_booted.is_ok() {
                self.booted += 1;
            }
            std::future::ready(self.mark_booted)
        }

        fn mark_updated(&mut self) -> impl Future<Output = Result<(), Self::Error>> {
            std::future::ready(self.do_mark_updated())
        }
    }
}
