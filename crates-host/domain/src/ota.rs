//! Узел `OTA`: принимает образ командами от узла протокола и применяет его.
//!
//! Здесь, а не в `crates-cross/app`, по общему правилу проекта
// {%- if graph == "true" %}
//! (см. `domain::heartbeat`)
// {%- endif %}
//! — что узел делает — логика, и проверяется она на хосте на фейках портов.
//!
//! Образ приходит не по своему каналу: узел протокола переводит свои сообщения в
//! [`OtaCommand`](ports::OtaCommand) и отдаёт их узлу через один из двух
//! входов: `CMD` (`request:` фрагмента — клиент ждёт исхода в вызове) или `ASK`
//! (`serves:` — клиент отправляет команду, не дожидаясь, и забирает исход из
//! `reply()`, обслуживая тем временем свой канал). Для узла входы одинаковы:
//! команды обоих идут в один автомат. На каждую команду узел отвечает
//! [`OtaReply`](ports::OtaReply) — её исходом, и протокол передаёт его хосту
//! как есть: ни опрашивать статус, ни угадывать, дошёл ли кусок, хосту не
//! нужно. Узел про протокол не знает, протокол — про флеш.
//!
//! Задача — автомат `#[fsm::typestate]` на контексте `OtaCtx`, который
//! эмитит фрагмент `OTA_FRAG`:
//!
//! * `Idle` — приёма нет; `Start` проверяет занятость и подпись и, если всё
//!   в порядке, стирает раздел под образ;
//! * `Receiving` — идёт приём от одного клиента
//!   ([`ClientId`](crate::ota::ClientId), транспорт): куски пишутся во флеш
//!   шагами [`Transfer`](crate::ota::download::Transfer), `Finish` применяет образ;
//!   `Start`/`Chunk`/`Finish` другого клиента получают `Busy`; владелец молчит
//!   дольше [`SESSION_TIMEOUT`](crate::ota::SESSION_TIMEOUT) — приём брошен;
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
// {%- if graph == "true" %}
//! [`Mode::Signed`](crate::ota::Mode::Signed), и выбирает его `crates-cross/app/src/graph.rs`.
// {%- else %}
//! [`Mode::Signed`](crate::ota::Mode::Signed), и выбирает его `crates-cross/app/src/tasks/ota.rs`.
// {%- endif %}
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

mod client;
mod rejection;
mod timing;
mod wiring;

// Типы входа `CMD` — под путём `domain`: фрагмент называет их полным путём, а
// compose-site (`app`) от `ports` не зависит.
pub use ports::{OtaCommand, OtaReply};

// Подмодули — по теме; путь `domain::ota::…` у их типов прежний.
pub use client::{CMD_BYTES, ClientId, NodeLost, OtaClient, OtaRequest};
pub use timing::{REBOOT_GRACE, SESSION_TIMEOUT};
pub use wiring::{Mode, OtaWiring};

use core::future::poll_fn;
use core::pin::pin;
use core::task::Poll;

use defmt_or_log::{FormatOrDebug, info, warn};
use embassy_time::{Instant, Timer};
use ports::{Chunk, FirmwareUpdate, Progress, Reboot, Rejection};

use crate::ota::client::Answer;
use crate::ota::download::Transfer;
use crate::ota::rejection::{busy, download_rejection};
use crate::ota::wiring::Signature;

// {%- if graph == "true" %}
// Узел объявлен здесь же, где лежит его задача, — как `HEARTBEAT_FRAG` в
// `domain::heartbeat`; compose-site (`crates-cross/app/src/graph.rs`) только
// перечисляет фрагмент, называет узел (`as OTA`) и кормит его входом. Фрагмент
// один на оба Cargo-варианта `signed`: режим применения — данные входа
// (`OtaWiring::mode`), а не второй фрагмент. Какой режим подставить — решает
// `graph.rs`.
// {%- else %}
// Узел объявлен здесь же, где лежит его задача; задача модуля
// `crates-cross/app/src/tasks/ota.rs` кормит его входом. Фрагмент один на оба
// Cargo-варианта `signed`: режим применения — данные входа (`OtaWiring::mode`),
// а не второй фрагмент. Какой режим подставить — решает `tasks/ota.rs`.
// {%- endif %}
//
// `boot: inputs: …` — собственная привязка фрагмента: compose-site пишет
// {%- if graph == "true" %}
// `fragments: [::domain::OTA_FRAG<…> as OTA = ::domain::ota::OtaWiring { … }]`,
// а `spawn_all` связывает её первой строкой пролога, и инициализаторы слотов
// {%- else %}
// `::domain::ota::OtaWiring { … }` (`crates-cross/app/src/tasks/ota.rs`),
// и инициализаторы слотов
// {%- endif %}
// ниже читают её поля. `CONFIRMED` — без входа: подтверждён ли уже образ в
// этом запуске (см. действие `begin`).
//
// `request: [CMD: …]` — вход команд: запрос с ответом (`sync-request`), а не
// очередь, потому что протокол отвечает хосту исходом именно своей команды.
// {%- if graph == "true" %}
// Узел протокола держит `OtaClient` — requester из аксессора графа
// `ota_cmd_requester()` и номер транспорта, который раздаёт compose-site.
// {%- else %}
// Узел протокола держит `OtaClient` — requester входа и номер транспорта,
// которые раздаёт `crates-cross/app/src/tasks/ota.rs`.
// {%- endif %}
// `serves: [ASK: …]` — тот же вход для клиента, которому нельзя ждать исхода в
// вызове: узел протокола, который всё это время должен обслуживать свой
// канал. Он объявляет ребро `asks: [OTA.ASK: …; квота tag …]`, отправляет
// команду, не дожидаясь, и забирает исход из `reply()`. Очередь — сумма квот
// рёбер, её считает `supervisor`; без спрашивающих она пустая, и `ASK` просто молчит.
// Ёмкость обоих — `CMD_BYTES` (литералом: макрос другого не принимает).
// {%- if graph != "true" %}
// Её же берут `static` входов (`crates-cross/app/src/tasks/ota.rs`).
// {%- endif %}
// Менять все места вместе.
//
// `<F, R>` — параметры фрагмента: адаптер обновления и порт перезагрузки
// платы `domain` не видит. `local` на `FLASH`: разделы платы — `!Send`
// (`adapters::ota::Updater` над ними держит `&'static Mutex<NoopRawMutex, …>`),
// а обычный слот — `static`, которому нужен `Send`. `local` меняет слот на
// `LocalResourceSlot` и снимает это требование, взамен обязывая держать
// значение на одном исполнителе — том, что спавнит задачи (то же допущение, что
// `NoopRawMutex` у `FlashMutex`). Compose-site платит фичей
// `supervisor/local-resources` и `unsafe`-блоком на каждый `local`-`static`.
//
// Без `watchdog:` намеренно — см. `task`.
//
// `ready` — образ подтверждён (или попытка была), `STATE` больше не стирается
// при старте (см. `begin`){% if has_usb == "true" %}; ждёт этого узел `USB`{% endif %}.
supervisor::supervisor_fragment! {
    name: OTA_FRAG<F, R>;
    boot: inputs: $crate::ota::OtaWiring<F, R>;

    node resources: [
            /// Адаптер обновления поверх разделов флеша (`FirmwareUpdate`).
            FLASH: local F = inputs.flash,
            /// Перезагрузка МК (`Reboot`) — для команды `Reboot`.
            REBOOT: R = inputs.reboot,
            /// Режим применения: `Mode::Plain` или `Mode::Signed`.
            MODE: $crate::ota::Mode = inputs.mode,
            CONFIRMED: bool = false
        ],
        // Литерал, а не `CMD_BYTES`: макрос принимает здесь только число. Равен ему.
        request: [CMD: $crate::ota::OtaRequest -> $crate::ota::OtaReply; 144],
        serves: [ASK: $crate::ota::OtaRequest -> $crate::ota::OtaReply; 144],
        ready,
        ctx: $crate::ota::OtaCtx,
        task: $crate::ota::task;
}

#[fsm::typestate]
impl<'a, F, R> OtaCtx<'a, F, R>
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
                /// Идёт приём образа от клиента `owner`; `until` — когда бросить
                /// его, если владелец замолчал.
                Receiving {
                    owner: ClientId,
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
                // команда владельца его не нарушила. Чужой `Start`, выигравший
                // гонку у просроченного срока, `start` сверяет с `until` сам.
                *                       => cmd: self.command(),
                Receiving { until, .. } => expired: fsm::deadline(*until),
            }
            transitions {
                // Чей приём — проверяют `start`/`chunk`/`finish`: чужому — `Busy`.
                *         + cmd((OtaRequest { client, command: OtaCommand::Start { len, signature } }, served)) => start -> Receiving | Idle | Ready,
                Receiving + cmd((OtaRequest { client, command: OtaCommand::Chunk(ref chunk) }, served))         => chunk -> Receiving | Idle,
                Receiving + cmd((OtaRequest { client, command: OtaCommand::Finish }, served))                   => finish -> Ready | Idle | Receiving,
                Idle      + cmd((OtaRequest { command: OtaCommand::Chunk(_) | OtaCommand::Finish, .. }, served)) => no_session -> self,
                Ready     + cmd((OtaRequest { command: OtaCommand::Chunk(_) | OtaCommand::Finish, .. }, served)) => no_session -> self,
                *         + cmd((OtaRequest { command: OtaCommand::Reboot, .. }, served))                       => reboot -> self,
                Receiving + expired(())                                                                         => abandon -> Idle,
            }
        }
    }

    /// Следующая команда с любого из двух входов — источник `cmd` автомата.
    ///
    /// Без потерь при дропе, как требует `listen`: `serve()` обоих входов
    /// забирает запрос только в том опросе, который его возвращает, а `CMD`
    /// опрашивается первым и, готовый, `ASK` уже не трогает — запрос, который
    /// не вернулся, остаётся в своей очереди.
    async fn command(&mut self) -> (OtaRequest, Answer<'a>) {
        let mut call = pin!(self.cmd.serve());
        let mut ask = pin!(self.ask.serve());
        poll_fn(|cx| {
            if let Poll::Ready((request, served)) = call.as_mut().poll(cx) {
                return Poll::Ready((request, Answer::Call(served)));
            }
            ask.as_mut()
                .poll(cx)
                .map(|(request, asked)| (request, Answer::Ask(asked)))
        })
        .await
    }

    /// Подтверждение образа, затем ожидание команд.
    ///
    /// Без подтверждения bootloader считает попытку неудачной и на следующем
    /// сбросе откатывает образ — обновление отработало бы ровно один запуск.
    /// Здесь, а не в `main` до исполнителя: порт асинхронный, и его реализация
    // {%- if ota_store == "external" %}
    /// (драйвер внешней флеши)
    // {%- endif %}
    /// может ждать таймер или прерывание, а это требует исполнителя. На
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
    ///
    /// Готовность (`ready`) — после подтверждения, удачного или нет.
    /// Первая загрузка нового или откаченного образа стирает здесь `STATE`,
    /// и на чипе, где `STATE` в одном банке с кодом, ядро на это время (~1 с
    /// на секторе 128K у H7) стоит на выборке инструкций; узлу, которому это
    /// мешает, хватит `deps: [OTA ready]{% if has_usb == "true" %}`. Так ждёт узел `USB`: перечисление,
    /// попавшее на стирание, хост бракует до переподключения (`docs/ota.md`).
    /// Переносите подтверждение позже — переносите и это{% else %}`{% endif %}{% comment %}`{% endcomment %}.
    async fn begin(&mut self) -> Idle {
        info!("domain: узел OTA запущен");
        self.confirm().await;
        self.ready.set();
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

    /// `Start`: проверки до стирания — владелец приёма, занятость, затем режим
    /// (подпись), — и подготовка раздела. Незаконченный приём того же клиента
    /// брошен, чужой — `Busy`.
    async fn start(
        &mut self,
        from: edges::StartFrom,
        client: ClientId,
        len: u32,
        signature: Option<Signature>,
        served: Answer<'_>,
    ) -> edges::Start {
        info!("ota: Start от клиента {}: {} байт", client.0, len);
        // Образ, ждущий обмена, затирать нельзя: `Busy`, и он ждёт дальше. Узел
        // знает это сам, не спрашивая адаптер, — и не зовёт подтверждение,
        // которое сняло бы запрос на обмен (см. `begin`).
        let back: edges::Start = match from {
            edges::StartFrom::Ready(Ready) => {
                warn!("ota: Start отвергнут: образ ждёт обмена, нужен Reboot");
                served.respond(Err(Rejection::Busy));
                return Ready.into();
            }
            // Приём другого клиента не трогаем — ни раздел, ни его срок; но
            // срок вышел (`expired` просто не успел) — приём брошен, как по
            // `abandon`, и это обычный `Start`.
            edges::StartFrom::Receiving(s) if s.owner != client && Instant::now() < s.until => {
                return busy(s, client, served).into();
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
                    owner: client,
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

    /// `Chunk`: кусок во флеш. Отказ — приём брошен, хост начинает заново;
    /// кусок не от владельца — `Busy`, не записан.
    async fn chunk(
        &mut self,
        mut s: Receiving,
        client: ClientId,
        chunk: &Chunk,
        served: Answer<'_>,
    ) -> edges::Chunk {
        if client != s.owner {
            return busy(s, client, served).into();
        }
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
    /// signed); не от владельца — `Busy`, приём продолжается.
    async fn finish(
        &mut self,
        s: Receiving,
        client: ClientId,
        served: Answer<'_>,
    ) -> edges::Finish {
        if client != s.owner {
            return busy(s, client, served).into();
        }
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
    fn no_session(&mut self, served: Answer<'_>) {
        warn!("ota: кусок или Finish без Start");
        served.respond(Err(Rejection::NoSession));
    }

    /// `Reboot`: ответ, пауза на его отправку, сброс.
    async fn reboot(&mut self, served: Answer<'_>) {
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
pub async fn task<F, R>(mut ctx: OtaCtx<'_, F, R>)
where
    F: FirmwareUpdate,
    F::Error: FormatOrDebug,
    R: Reboot,
{
    match ctx.run().await {}
}

#[cfg(test)]
pub(crate) mod tests;

#[cfg(test)]
mod fakes;
