//! Логика узла OTA: приём образа, typestate-состояния и применение.
//!
//! Задача [`task`](super::task) собирает [`Ota`] из полей контекста и крутит
//! его цикл; здесь — что узел делает с командой, тактом и сроком молчания.

use core::future::poll_fn;
use core::pin::pin;
use core::task::Poll;

use defmt_or_log::{info, warn};
use embassy_time::{Instant, Timer};
use embedded_storage_async::nor_flash::NorFlash;
use ports::{Partitions, Reboot};

use super::client::Answer;
use super::command::{Chunk, Progress, Rejection, SIGNATURE_BYTES};
use super::download::Transfer;
use super::rejection::{busy, download_rejection, update_rejection};
use super::timing::{REBOOT_GRACE, SESSION_TIMEOUT};
use super::updater::Updater;
use super::{ClientId, OtaCommand, OtaReply, OtaRequest, update};

/// Подпись образа — из `Start`, до `Finish` её держит состояние `Receiving`.
type Signature = [u8; SIGNATURE_BYTES];

/// Узел OTA: адаптер обновления, порт перезагрузки и оба входа команд.
pub(super) struct Ota<'a, F, R> {
    /// Адаптер обновления поверх разделов флеша (`Updater`).
    flash: &'a mut F,
    /// Перезагрузка МК (`Reboot`) — для команды `Reboot`.
    reboot: &'a mut R,
    /// Версия этой прошивки: образ не новее неё отвергается на `Finish`.
    running_version: &'a mut u32,
    /// Подтверждён ли образ в этом запуске.
    confirmed: &'a mut bool,
    /// Вход `CMD`: запрос с ответом.
    cmd: supervisor::Server<'a, OtaRequest, OtaReply>,
    /// Вход `ASK`: команда без ожидания исхода.
    ask: supervisor::AskServer<'a, OtaRequest, OtaReply>,
    /// Готовность узла: подтверждение образа выполнено.
    ready: supervisor::Readiness,
}

impl<'a, F, R> Ota<'a, F, R> {
    /// Собирает логику узла из полей контекста.
    #[must_use]
    pub(super) const fn new(
        flash: &'a mut F,
        reboot: &'a mut R,
        running_version: &'a mut u32,
        confirmed: &'a mut bool,
        cmd: supervisor::Server<'a, OtaRequest, OtaReply>,
        ask: supervisor::AskServer<'a, OtaRequest, OtaReply>,
        ready: supervisor::Readiness,
    ) -> Self {
        Self {
            flash,
            reboot,
            running_version,
            confirmed,
            cmd,
            ask,
            ready,
        }
    }
}

/// Состояние узла в цикле задачи.
///
/// Typestate: переход возможен только через consuming-метод текущего
/// состояния, а вызвать его можно только из ветки `State::on_command` под
/// свою команду. Так таблица переходов остаётся исчерпываемой: пары
/// (состояние, команда) без перехода просто не пишутся, а компилятор ловит
/// попытку собрать, скажем, [`Ready`] из чужого `Finish`.
#[derive(Debug)]
pub enum State {
    /// Приёма нет.
    Idle(Idle),
    /// Идёт приём.
    Receiving(Receiving),
    /// Образ применён, обмен разделов ждёт сброса.
    Ready(Ready),
}

/// Приёма нет.
#[derive(Debug)]
pub struct Idle;

/// Идёт приём образа от клиента `owner`; `until` — когда бросить
/// его, если владелец замолчал.
#[derive(Debug)]
pub struct Receiving {
    pub(super) owner: ClientId,
    transfer: Transfer,
    signature: Signature,
    until: Instant,
}

/// Образ применён, обмен разделов — на следующем сбросе; `owner` —
/// чей `Finish` его применил: его повтор снова получает `Ready`.
#[derive(Debug)]
pub struct Ready {
    owner: ClientId,
}

impl State {
    /// Срок молчания владельца приёма, если он есть.
    pub(super) fn deadline(&self) -> Option<Instant> {
        match self {
            Self::Receiving(s) => Some(s.until),
            _ => None,
        }
    }

    /// Команда — по переходу текущего состояния: разводка
    /// (состояние × команда). `Chunk` без приёма, `Finish` владельца и т.п. —
    /// ветки только там, где переход есть; компилятор не даст вызвать
    /// `Receiving::chunk` из `Ready`.
    pub(super) async fn on_command<'a, D, S, R>(
        self,
        io: &mut Ota<'a, Updater<Partitions<D, S>>, R>,
        client: ClientId,
        command: OtaCommand,
        served: Answer<'a>,
    ) -> State
    where
        D: NorFlash,
        S: NorFlash,
    {
        match command {
            OtaCommand::Start { len, signature } => match self {
                State::Idle(s) => s.start(io, client, len, signature, served).await,
                State::Receiving(s) => s.start(io, client, len, signature, served).await,
                State::Ready(s) => s.start(client, served),
            },
            OtaCommand::Chunk(chunk) => match self {
                State::Idle(s) => s.chunk(served),
                State::Receiving(s) => s.chunk(io, client, &chunk, served).await,
                State::Ready(s) => s.chunk(served),
            },
            OtaCommand::Finish => match self {
                State::Idle(s) => s.finish(served),
                State::Receiving(s) => s.finish(io, client, served).await,
                State::Ready(s) => s.finish(client, served),
            },
            OtaCommand::Reboot => unreachable!("`Reboot` разбирается в цикле, до разводки"),
        }
    }
}

impl Idle {
    /// `Start`: занятость и подпись, затем подготовка раздела — в
    /// [`Receiving`](State::Receiving) или, по отказу, назад сюда.
    async fn start<'a, D, S, R>(
        self,
        io: &mut Ota<'a, Updater<Partitions<D, S>>, R>,
        client: ClientId,
        len: u32,
        signature: Option<Signature>,
        served: Answer<'a>,
    ) -> State
    where
        D: NorFlash,
        S: NorFlash,
    {
        io.accept(client, len, signature, served).await
    }

    /// `Chunk` без приёма: отказ, состояние то же.
    fn chunk(self, served: Answer<'_>) -> State {
        no_session(served);
        State::Idle(self)
    }

    /// `Finish` без приёма: отказ, состояние то же.
    fn finish(self, served: Answer<'_>) -> State {
        no_session(served);
        State::Idle(self)
    }
}

impl Receiving {
    /// `Start` владельца или после срока — обычный приём (незаконченный
    /// приём брошен); чужого клиента в срок — `Busy`, приём продолжается.
    async fn start<'a, D, S, R>(
        self,
        io: &mut Ota<'a, Updater<Partitions<D, S>>, R>,
        client: ClientId,
        len: u32,
        signature: Option<Signature>,
        served: Answer<'a>,
    ) -> State
    where
        D: NorFlash,
        S: NorFlash,
    {
        // Приём другого клиента не трогаем — ни раздел, ни его срок; но
        // срок вышел (молчание дольше `until` обогнала команда) — приём
        // брошен, как по `expired`, и это обычный `Start`.
        if self.owner != client && Instant::now() < self.until {
            return State::Receiving(busy(self, client, served));
        }
        warn!("ota: незаконченный приём брошен новым Start");
        io.accept(client, len, signature, served).await
    }

    /// `Chunk`: кусок во флеш. Отказ — приём брошен, хост начинает заново;
    /// кусок не от владельца — `Busy`, не записан.
    async fn chunk<'a, D, S, R>(
        mut self,
        io: &mut Ota<'a, Updater<Partitions<D, S>>, R>,
        client: ClientId,
        chunk: &Chunk,
        served: Answer<'a>,
    ) -> State
    where
        D: NorFlash,
        S: NorFlash,
    {
        if client != self.owner {
            return State::Receiving(busy(self, client, served));
        }
        match self
            .transfer
            .chunk(&mut *io.flash, chunk.offset(), chunk.as_bytes())
            .await
        {
            Ok(received) => {
                served.respond(Ok(Progress::Received(received)));
                self.until = Instant::now() + SESSION_TIMEOUT;
                State::Receiving(self)
            }
            Err(err) => {
                warn!("ota: кусок отвергнут, приём брошен: {:?}", &err);
                served.respond(Err(download_rejection(&err)));
                State::Idle(Idle)
            }
        }
    }

    /// `Finish`: сверка длины, затем применение — проверки и запись подписи
    /// для bootloader'а ([`apply`](crate::ota::update::apply));
    /// не от владельца — `Busy`, приём продолжается.
    async fn finish<'a, D, S, R>(
        self,
        io: &mut Ota<'a, Updater<Partitions<D, S>>, R>,
        client: ClientId,
        served: Answer<'a>,
    ) -> State
    where
        D: NorFlash,
        S: NorFlash,
    {
        if client != self.owner {
            return State::Receiving(busy(self, client, served));
        }
        let outcome = match self.transfer.finish(&mut *io.flash).await {
            Ok(len) => update::apply(&mut *io.flash, *io.running_version, &self.signature, len)
                .await
                .map_err(|err| {
                    warn!("ota: применение отвергнуто: {:?}", &err);
                    update_rejection(&err)
                }),
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
                *io.confirmed = true;
                info!("ota: образ принят, обмен разделов на следующем сбросе");
                served.respond(Ok(Progress::Ready));
                State::Ready(Ready { owner: client })
            }
            Err(rejection) => {
                warn!("ota: образ отвергнут: {:?}", rejection);
                served.respond(Err(rejection));
                State::Idle(Idle)
            }
        }
    }

    /// Хост замолчал посреди образа: приём брошен.
    #[expect(
        clippy::unused_self,
        reason = "сигнатура перехода: состояние потребляется целиком"
    )]
    pub(super) fn expired(self) -> State {
        warn!("ota: хост молчит дольше тайм-аута — приём брошен");
        State::Idle(Idle)
    }
}

impl Ready {
    /// `Start` после применения: образ ждёт обмена, затирать его нельзя.
    /// Узел знает это сам, не спрашивая адаптер, — и не зовёт подтверждение,
    /// которое сняло бы запрос на обмен (см. `begin`).
    fn start(self, _client: ClientId, served: Answer<'_>) -> State {
        warn!("ota: Start отвергнут: образ ждёт обмена, нужен Reboot");
        served.respond(Err(Rejection::Busy));
        State::Ready(self)
    }

    /// `Chunk` после применения: отказ, состояние то же.
    fn chunk(self, served: Answer<'_>) -> State {
        no_session(served);
        State::Ready(self)
    }

    /// Повтор `Finish`: владельцу — снова `Ready`, ничего не
    /// записано и не стёрто. Ответ на первый мог потеряться (`Finish` пишет
    /// запись для bootloader'а и стирает `STATE` — на F4 до секунды), и
    /// `NoSession` здесь хост счёл бы провалом и не прислал бы `Reboot`, а
    /// образ молча применился бы на первом сбросе. Чужому — `NoSession`, как
    /// любой `Finish` без своего приёма.
    fn finish(self, client: ClientId, served: Answer<'_>) -> State {
        if client == self.owner {
            info!("ota: повтор Finish — образ уже принят");
            served.respond(Ok(Progress::Ready));
        } else {
            no_session(served);
        }
        State::Ready(self)
    }
}

impl<'a, D, S, R> Ota<'a, Updater<Partitions<D, S>>, R>
where
    D: NorFlash,
    S: NorFlash,
{
    /// Следующая команда с любого из двух входов.
    ///
    /// Без потерь при дропе, как требует гонка в цикле: `serve()` обоих
    /// входов забирает запрос только в том опросе, который его возвращает, а
    /// `CMD` опрашивается первым и, готовый, `ASK` уже не трогает — запрос,
    /// который не вернулся, остаётся в своей очереди.
    pub(super) async fn command(&mut self) -> (OtaRequest, Answer<'a>) {
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
    /// Здесь, а не в `main` до исполнителя, и один раз за запуск (`confirmed`
    /// — слот) — docs/ota.md, «Подтверждение образа — в узле OTA, один раз за
    /// запуск». На обычной загрузке вызов ничего не меняет (состояние и так
    /// `Boot`).
    ///
    /// Место — самое раннее из возможных, и это компромисс шаблона: он не
    /// знает, что для устройства значит «работает». Появится критерий
    /// (подняли связь, отработали цикл измерений) — переносите подтверждение
    /// туда; тогда откат застрахует и от образа, который стартует, но не
    /// работает.
    ///
    /// Ошибка — не повод ронять узел: флаг не ставится, узел пробует ещё раз
    /// на следующем `Start`, а следующий сброс без подтверждения вернёт
    /// прежний образ.
    ///
    /// Готовность (`ready`) — после подтверждения, удачного или нет.
    /// Первая загрузка нового или откаченного образа стирает здесь `STATE`,
    /// и на чипе, где `STATE` в одном банке с кодом, ядро на это время (~1 с
    /// на секторе 128K у H7) стоит на выборке инструкций; узлу, которому это
    /// мешает, хватит `deps: [OTA ready]{% if has_usb == "true" %}`. Так ждёт узел `USB`: перечисление,
    /// попавшее на стирание, хост бракует до переподключения (`docs/ota.md`).
    /// Переносите подтверждение позже — переносите и это{% else %}`{% endif %}{% comment %}`{% endcomment %}.
    pub(super) async fn begin(&mut self) {
        info!("ota: узел запущен");
        self.confirm().await;
        self.ready.set();
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

    /// Общая часть `Start`: проверки до стирания — занятость, затем
    /// подпись, — и подготовка раздела. Без подписи — отказ ДО стирания:
    /// применить такой образ нечем, а стирание уничтожило бы образ, в который
    /// устройство откатывается. Владельца приёма вызывающий проверил сам:
    /// сюда попадают только команды, которым можно стирать раздел.
    async fn accept(
        &mut self,
        client: ClientId,
        len: u32,
        signature: Option<Signature>,
        served: Answer<'_>,
    ) -> State {
        info!("ota: Start от клиента {}: {} байт", client.0, len);
        // Не удалось подтвердить при старте — ещё раз перед новым образом, а
        // не чаще: неудавшийся `mark_booted` может стоить стирания `STATE`.
        self.confirm().await;
        // Занятость — до всего: адаптер и так откажет в `prepare`, но тогда
        // хост услышал бы `Device` вместо «сначала перезагрузи».
        let screened = match self.flash.is_busy().await {
            Ok(false) => signature.ok_or(Rejection::Signature),
            Ok(true) => Err(Rejection::Busy),
            Err(err) => {
                warn!("ota: флеш не ответил о занятости: {:?}", &err);
                Err(Rejection::Device)
            }
        };
        let signature = match screened {
            Ok(signature) => signature,
            Err(rejection) => {
                warn!("ota: Start отвергнут: {:?}", rejection);
                served.respond(Err(rejection));
                return State::Idle(Idle);
            }
        };
        match Transfer::start(&mut *self.flash, len).await {
            Ok(transfer) => {
                served.respond(Ok(Progress::Started));
                State::Receiving(Receiving {
                    owner: client,
                    transfer,
                    signature,
                    until: Instant::now() + SESSION_TIMEOUT,
                })
            }
            Err(err) => {
                warn!("ota: Start отвергнут: {:?}", &err);
                served.respond(Err(download_rejection(&err)));
                State::Idle(Idle)
            }
        }
    }
}

impl<D, S, R> Ota<'_, Updater<Partitions<D, S>>, R>
where
    D: NorFlash,
    S: NorFlash,
    R: Reboot,
{
    /// `Reboot`: ответ, пауза на его отправку, сброс.
    pub(super) async fn reboot(&mut self, served: Answer<'_>) {
        info!("ota: перезагрузка по команде хоста");
        served.respond(Ok(Progress::Rebooting));
        Timer::after(REBOOT_GRACE).await;
        self.reboot.reboot()
    }
}

/// `Chunk` или `Finish` без приёма: отказ, состояние то же.
fn no_session(served: Answer<'_>) {
    warn!("ota: кусок или Finish без Start");
    served.respond(Err(Rejection::NoSession));
}
