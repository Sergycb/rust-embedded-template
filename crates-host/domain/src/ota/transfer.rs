//! Передача и применение образа: автомат узла, приём по кускам и проверки
//! перед обменом разделов.
//!
//! Задача [`task`](super::task) собирает [`Ota`] из полей контекста и крутит
//! его цикл; здесь — что узел делает с командой, тактом и сроком молчания, и
//! во что превращается принятый образ.
//!
//! Образ приходит командами ([`Command`]): узел протокола переводит свои
//! сообщения в `Start`, `Chunk` и `Finish`, а узел [`ota`](crate::ota)
//! ведёт приём шагами [`Transfer`]. Вокруг этих шагов есть работа, которая от
//! протокола не зависит вовсе, и делать её в каждом проекте заново незачем:
//!
//! * длину, пришедшую из недоверенного канала, надо сверить с тем, что
//!   устройство вообще способно принять, — до того, как стирать флеш;
//! * раздел надо подготовить ровно один раз, а не на каждый кусок;
//! * флеш принимает запись только словами (от байта у внешней NOR до тридцати
//!   двух у H7), а куски приходят какой угодно длины — значит
//!   кто-то должен копить хвост до кратности;
//! * смещение надо вести самому и не дать образу вылезти за обещанную длину;
//! * последний кусок почти наверняка неполный, и его надо дописать, добив до
//!   слова.
//!
//! [`Transfer`] делает всё перечисленное. Это не самостоятельный объект, а
//! данные состояния `Receiving` автомата узла OTA: между двумя командами приём
//! живёт там, а шаги — функции над адаптером [`Updater`]. Здесь, а не в
//! `bsp`: это правила приёма, а не свойство флеша, и проверяются они на
//! хосте — над разделами в памяти, на которых можно разыграть и лишний байт, и
//! кусок в один байт.
//!
//! # Автомат узла
//!
//! Typestate: переход возможен только через consuming-метод текущего
//! состояния, а вызвать его можно только из ветки `State::on_command` под свою
//! команду. Так таблица переходов остаётся исчерпываемой: пары (состояние,
//! команда) без перехода просто не пишутся, а компилятор ловит попытку
//! собрать, скажем, [`Ready`] из чужого `Finish`.
//!
//! # Применение образа
//!
//! Здесь, а не в адаптере: «принимать ли этот образ» — правило продукта, и
//! проверяется оно на хосте над разделами в памяти. Подпись приложение не
//! проверяет — это делает bootloader (`domain::ota::boot::screen_update`), а
//! отсюда ему уходит запись с подписью и длиной ([`PendingImage`]) — обычной
//! записью адаптера в запасную страницу раздела.
//!
//! Хвост образа — `[образ][раскладка u32 LE][версия u32 LE]`; подпись, хвост
//! и порядок проверок — docs/ota.md, «Подпись OTA-образа».

use core::future::poll_fn;
use core::pin::pin;
use core::task::Poll;

use defmt_or_log::{info, warn};
use embassy_time::{Duration, Instant, Timer};
use embedded_storage_async::nor_flash::NorFlash;
use ports::{MAX_WRITE_SIZE, Partitions, PendingImage, Reboot};

use super::Answer;
use super::commands::{Chunk, Progress, Rejection, SIGNATURE_BYTES};
use super::storage::{Updater, UpdaterError};
use super::{ClientId, Command, Reply, Request};
use crate::ota::version;

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

/// Сколько байт в хвосте образа занимает версия.
///
/// Хвост, а не фиксированное смещение внутри образа (почему — docs/ota.md,
/// «Подпись OTA-образа»). Те же четыре байта дописывает `cargo xtask build`,
/// читая версию из символа `FW_VERSION_IN_IMAGE` в ELF (`crates-host/xtask`).
pub const VERSION_BYTES: u32 = 4;

/// Сколько байт в хвосте образа занимает отпечаток раскладки — прямо перед
/// версией ([`VERSION_BYTES`]).
///
/// Перед версией, а не за ней: версия осталась на `len - 4`, где её и
/// читали (`apply`, target-тест `ota_rejects_an_older_image`).
/// `cargo xtask build` берёт значения символов [`ports::LAYOUT_SYMBOLS`] из
/// ELF приложения.
pub const LAYOUT_BYTES: u32 = 4;

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
    cmd: supervisor::Server<'a, Request, Reply>,
    /// Вход `ASK`: команда без ожидания исхода.
    ask: supervisor::AskServer<'a, Request, Reply>,
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
        cmd: supervisor::Server<'a, Request, Reply>,
        ask: supervisor::AskServer<'a, Request, Reply>,
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
        command: Command,
        served: Answer<'a>,
    ) -> State
    where
        D: NorFlash,
        S: NorFlash,
    {
        match command {
            Command::Start { len, signature } => match self {
                State::Idle(s) => s.start(io, client, len, signature, served).await,
                State::Receiving(s) => s.start(io, client, len, signature, served).await,
                State::Ready(s) => s.start(client, served),
            },
            Command::Chunk(chunk) => match self {
                State::Idle(s) => s.chunk(served),
                State::Receiving(s) => s.chunk(io, client, &chunk, served).await,
                State::Ready(s) => s.chunk(served),
            },
            Command::Finish => match self {
                State::Idle(s) => s.finish(served),
                State::Receiving(s) => s.finish(io, client, served).await,
                State::Ready(s) => s.finish(client, served),
            },
            Command::Reboot => unreachable!("`Reboot` разбирается в цикле, до разводки"),
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
    /// для bootloader'а ([`apply`]);
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
            Ok(len) => apply(&mut *io.flash, *io.running_version, &self.signature, len)
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
    pub(super) async fn command(&mut self) -> (Request, Answer<'a>) {
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

/// Приём одного образа между `Start` и `Finish` — данные состояния
/// `Receiving` узла OTA.
#[derive(Debug, Clone)]
pub struct Transfer {
    /// Обещанная длина образа.
    announced: u32,
    /// Слово записи флеша — проверено в [`start`](Self::start).
    word: u32,
    /// Сколько байт уже во флеше — всегда кратно слову.
    written: u32,
    /// Хвост, не добравший до целого слова (буфер на самое широкое слово
    /// чипа: гранулярность известна только в рантайме). Первые `pending` байт
    /// значимы.
    tail: [u8; MAX_WRITE_SIZE],
    /// Сколько байт в `tail` — меньше слова.
    pending: usize,
}

impl Transfer {
    /// Начинает приём образа длиной `announced`: проверяет длину и готовит
    /// раздел.
    ///
    /// Порядок важен: длина сверяется с вместимостью ДО стирания, потому что
    /// стирание уничтожает образ, в который устройство откатывается. Узнать,
    /// что длина негодная, после этого — значит остаться без обеих прошивок.
    ///
    /// # Errors
    ///
    /// - `Empty` — заявлена нулевая длина,
    /// - `TooLong` — длина больше вместимости раздела,
    /// - `Flash` — флеш отказал.
    pub async fn start<D: NorFlash, S: NorFlash>(
        flash: &mut Updater<Partitions<D, S>>,
        announced: u32,
    ) -> Result<Self, DownloadError> {
        if announced == 0 {
            return Err(DownloadError::Empty);
        }

        let capacity = flash.capacity();
        if announced > capacity {
            return Err(DownloadError::TooLong {
                announced,
                capacity,
            });
        }

        // Слово — от 1 до `MAX_WRITE_SIZE`, и делить на него можно; раздел
        // кратен странице, страница — слову, так что и добитый до слова хвост
        // не выходит за раздел. Всё это проверил `Updater::new` при сборке.
        let word = flash.write_granularity();

        flash
            .prepare(announced)
            .await
            .map_err(DownloadError::Flash)?;

        Ok(Self {
            announced,
            word,
            written: 0,
            tail: [0; MAX_WRITE_SIZE],
            pending: 0,
        })
    }

    /// Сколько байт образа принято — записано и ждёт в хвосте.
    #[must_use]
    pub fn received(&self) -> u32 {
        self.written + self.pending_u32()
    }

    /// Принимает кусок по смещению `offset` и возвращает, сколько байт принято
    /// всего.
    ///
    /// Кусок, целиком лежащий в уже принятом, — повтор (ответ на него
    /// потерялся, и отправитель послал снова): подтверждается без записи.
    /// Во флеш уходят только целые слова; остаток куска ждёт следующего.
    /// После ошибки приём продолжать нельзя: хвост и смещение уже не
    /// сходятся с тем, что во флеше, — узел бросает его.
    ///
    /// # Errors
    ///
    /// - `OutOfOrder` — кусок не встык к принятым: разрыв или наложение,
    /// - `TooMuchData` — кусок выводит образ за обещанную длину,
    /// - `Flash` — флеш отказал.
    ///
    /// # Panics
    ///
    /// Если кусок длиннее `u32::MAX`: размеры образов ограничены разделом
    /// флеша, на практике это невозможно.
    pub async fn chunk<D: NorFlash, S: NorFlash>(
        &mut self,
        flash: &mut Updater<Partitions<D, S>>,
        offset: u32,
        mut chunk: &[u8],
    ) -> Result<u32, DownloadError> {
        let len = u32::try_from(chunk.len()).expect("кусок короче u32::MAX байт");
        let expected = self.received();
        if offset != expected {
            return if offset.saturating_add(len) <= expected {
                Ok(expected)
            } else {
                Err(DownloadError::OutOfOrder { expected, offset })
            };
        }
        let received = expected.saturating_add(len);
        if received > self.announced {
            return Err(DownloadError::TooMuchData {
                announced: self.announced,
                received,
            });
        }

        let word_len = self.word as usize;
        // Сначала добить хвост прошлого куска: пока он неполон, писать во флеш
        // нечего, а порядок байт в образе обязан сохраниться.
        if self.pending > 0 {
            let take = (word_len - self.pending).min(chunk.len());
            self.tail[self.pending..self.pending + take].copy_from_slice(&chunk[..take]);
            self.pending += take;
            chunk = &chunk[take..];

            if self.pending < word_len {
                return Ok(received);
            }
            flash
                .write(self.written, &self.tail[..word_len])
                .await
                .map_err(DownloadError::Flash)?;
            self.written += self.word;
            self.pending = 0;
        }

        // Целые слова — прямо во флеш, без копирования через буфер.
        let whole = chunk.len() - chunk.len() % word_len;
        if whole > 0 {
            flash
                .write(self.written, &chunk[..whole])
                .await
                .map_err(DownloadError::Flash)?;
            self.written += u32::try_from(whole).expect("кусок короче u32::MAX байт");
        }

        // Остаток короче слова ждёт следующего куска.
        let rest = &chunk[whole..];
        self.tail[..rest.len()].copy_from_slice(rest);
        self.pending = rest.len();
        Ok(received)
    }

    /// Заканчивает приём и возвращает длину образа — её ждёт применение
    /// обновления.
    ///
    /// Последнее неполное слово добивается единицами — фиксированным
    /// значением, и только поэтому: называть `0xFF` «стёртым состоянием
    /// флеша» нельзя, на L0/L1 оно нулевое (docs/flash.md), и там добор — самая
    /// обычная запись.
    ///
    /// # Errors
    ///
    /// - `Incomplete` — принято меньше обещанного,
    /// - `Flash` — флеш отказал.
    pub async fn finish<D: NorFlash, S: NorFlash>(
        mut self,
        flash: &mut Updater<Partitions<D, S>>,
    ) -> Result<u32, DownloadError> {
        let received = self.received();
        if received != self.announced {
            return Err(DownloadError::Incomplete {
                announced: self.announced,
                received,
            });
        }

        if self.pending > 0 {
            let word_len = self.word as usize;
            self.tail[self.pending..word_len].fill(0xFF);
            flash
                .write(self.written, &self.tail[..word_len])
                .await
                .map_err(DownloadError::Flash)?;
        }
        Ok(self.announced)
    }

    /// `pending` в `u32`: он меньше слова, то есть не больше 32.
    #[expect(
        clippy::cast_possible_truncation,
        reason = "pending меньше слова флеша (не больше 32 байт)"
    )]
    fn pending_u32(&self) -> u32 {
        self.pending as u32
    }
}

/// Что может пойти не так при приёме образа ([`Transfer`]).
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum DownloadError {
    /// Обещали образ нулевой длины. Проверяется здесь, а не оставляется
    /// реализации порта: `prepare(0)` не стёр бы ничего, и первая же запись
    /// легла бы в нестёртую память, на F2/F4/F7 молча.
    Empty,
    /// Устройство столько не примет: образ длиннее того, что помещается в
    /// раздел. Проверяется до стирания — раздел остаётся нетронутым.
    TooLong { announced: u32, capacity: u32 },
    /// Прислали больше, чем обещали в начале. Приём прекращается: это либо
    /// сбой канала, либо попытка вылезти за раздел.
    TooMuchData { announced: u32, received: u32 },
    /// Кусок не встык к принятым: разрыв или наложение. Повтор уже принятого
    /// куска сюда не попадает — его подтверждают без записи.
    OutOfOrder { expected: u32, offset: u32 },
    /// Передача кончилась, не добрав до обещанной длины.
    Incomplete { announced: u32, received: u32 },
    /// Отказ самого флеша.
    Flash(UpdaterError),
}

/// Чем может закончиться применение обновления ([`apply`]).
///
/// Каждому отказу — свой вариант, и это не для красоты: пока разные защиты
/// возвращали одно и то же, target-тест оставался зелёным после удаления одной
/// из них (проверено удалением на живой плате). Отказов подписи здесь нет:
/// её проверяет bootloader, и о неудаче отправитель узнаёт по тому, что версия
/// после перезагрузки не сменилась.
#[derive(Debug, PartialEq, Eq)]
#[cfg_attr(feature = "defmt", derive(defmt::Format))]
#[non_exhaustive]
pub enum UpdateError {
    /// Длина из недоверенного канала больше вместимости. Проверяется ПЕРВОЙ:
    /// длина уходит в запись для bootloader'а, и образ, не помещающийся в
    /// `ACTIVE`, он отверг бы только после перезагрузки.
    TooLong { len: u32, capacity: u32 },
    /// Образ короче собственного хвоста (раскладка и версия) — мусор в канале,
    /// а не откат.
    Truncated { len: u32 },
    /// В разделе обновления лежит образ отката — трогать его нельзя. Отделён от
    /// [`Rollback`](Self::Rollback): версия у того образа заведомо старее, и
    /// проверка версии честно ответила бы «откат», соврав о причине.
    Busy,
    /// Образ не новее текущего — попытка отката. Обе версии в упакованном
    /// виде (см. [`version`]).
    Rollback { current: u32, incoming: u32 },
    /// Образ собран под другую раскладку памяти: отпечаток в его хвосте не
    /// совпал с отпечатком этой прошивки (см. [`ports::layout_fingerprint`]).
    /// Обмен не запрашивается — такой образ после него не запустился бы.
    Layout { device: u32, image: u32 },
    /// Всё, что приходит из адаптера: отказ флеша, негодное состояние.
    Flash(UpdaterError),
}

impl From<UpdaterError> for UpdateError {
    fn from(error: UpdaterError) -> Self {
        Self::Flash(error)
    }
}

/// Сверяет отпечаток раскладки в хвосте принятого образа с отпечатком этой
/// прошивки ([`Updater::layout`]) — до `mark_updated`.
///
/// Образ под другую раскладку (другие `ACTIVE`/`DFU`/`STATE` в `memory.x`)
/// после обмена не запустился бы, а bootloader, собранный под старую, обменял
/// бы не те страницы. Отказать здесь — последний момент, когда обмен ещё не
/// запрошен. Подпись тут не нужна: защита от ошибки сборки, а не от
/// подделки.
///
/// # Errors
///
/// - `Truncated` — если длина меньше хвоста с раскладкой и версией,
/// - `Layout` — если отпечатки не совпали,
/// - `Flash` — если чтение раздела не удалось.
async fn check_layout<D: NorFlash, S: NorFlash>(
    flash: &mut Updater<Partitions<D, S>>,
    len: u32,
) -> Result<(), UpdateError> {
    // Отдельная ветка, чтобы вычитание не ушло в отрицательное: в release оно
    // завернулось бы и увело чтение в конец раздела.
    let Some(offset) = len.checked_sub(LAYOUT_BYTES + VERSION_BYTES) else {
        return Err(UpdateError::Truncated { len });
    };
    // Выравнивать смещение не нужно: у `embassy-stm32` `READ_SIZE = 1` — одна
    // константа на все семейства, — так что чтение четырёх байт с любого
    // адреса законно. Появись у чипа требование пожёстче, здесь пришлось бы
    // дочитывать блок целиком.
    let mut raw = [0u8; LAYOUT_BYTES as usize];
    flash.read(offset, &mut raw).await?;
    // Порядок байт — тот же, что пишет xtask: little-endian, как у Cortex-M.
    let image = u32::from_le_bytes(raw);
    let device = flash.layout();
    if image != device {
        return Err(UpdateError::Layout { device, image });
    }
    Ok(())
}

/// Проверяет принятый образ и, если всё сошлось, оставляет bootloader'у
/// подпись и просит поменять разделы местами на следующем сбросе. Сам сброс —
/// за вызывающим.
///
/// `signature` и `len` приезжают вместе с образом по вашему каналу, а берутся
/// из сборки: `cargo xtask build` кладёт рядом с ELF сырой `app.bin`, его
/// подпись `app.bin.sig` и печатает длину. Подписан не образ, а его SHA-512.
/// `running_version` — версия этой прошивки (свойства образа приложения).
///
/// Запись о подписи ложится в запасную страницу раздела, по смещению
/// [`capacity`](Updater::capacity) — её стирает [`Updater::prepare`], — и
/// только потом помечается обмен: bootloader, увидев запрос, обязан найти и
/// запись.
///
/// Порядок проверок — длина (с [`capacity`](Updater::capacity),
/// то есть `ACTIVE`), занятость раздела, раскладка (`check_layout`), версия —
/// ради различимых отказов; причина каждого шага — docs/ota.md, «Подпись
/// OTA-образа». Подпись здесь не проверяется: это делает bootloader перед
/// обменом.
///
/// # Errors
///
/// Возвращает ошибку:
/// - `TooLong` — если длина больше вместимости раздела,
/// - `Busy` — если раздел обновления занят образом, ждущим обмена или отката,
/// - `Truncated` — если длина меньше хвоста с раскладкой и версией,
/// - `Layout` — если образ собран под другую раскладку памяти,
/// - `Rollback` — если версия не новее текущей,
/// - `Flash` — если адаптер флеша вернул ошибку.
pub async fn apply<D: NorFlash, S: NorFlash>(
    flash: &mut Updater<Partitions<D, S>>,
    running_version: u32,
    signature: &[u8; 64],
    len: u32,
) -> Result<(), UpdateError> {
    let capacity = flash.capacity();
    if len > capacity {
        return Err(UpdateError::TooLong { len, capacity });
    }

    if flash.is_busy().await? {
        return Err(UpdateError::Busy);
    }

    check_layout(flash, len).await?;

    // Не уйдёт в отрицательное: `check_layout` уже отказал образу короче
    // хвоста. Чтение — как там: любой адрес, little-endian.
    let offset = len - VERSION_BYTES;
    let mut raw = [0u8; VERSION_BYTES as usize];
    flash.read(offset, &mut raw).await?;
    let incoming = u32::from_le_bytes(raw);
    let current = running_version;
    if !version::is_upgrade(current, incoming) {
        return Err(UpdateError::Rollback { current, incoming });
    }

    let record = PendingImage {
        len,
        signature: *signature,
    };
    flash.write(capacity, &record.to_bytes()).await?;
    flash.mark_updated().await?;
    Ok(())
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

/// `Chunk` или `Finish` без приёма: отказ, состояние то же.
fn no_session(served: Answer<'_>) {
    warn!("ota: кусок или Finish без Start");
    served.respond(Err(Rejection::NoSession));
}

#[cfg(test)]
#[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
mod download_tests {
    use embedded_storage::nor_flash::NorFlashErrorKind;
    use embedded_storage_async::nor_flash::NorFlash;
    use ports::{MAX_WRITE_SIZE, Partitions};

    use super::{DownloadError, Transfer};
    use crate::ota::fake::{flash, flash_with_word};
    use crate::ota::{Updater, UpdaterError};

    /// Исход приёма целиком и сколько кусков в него ушло.
    struct Received {
        result: Result<u32, DownloadError>,
        taken: usize,
    }

    /// Приём целиком, как его ведёт узел: `start`, куски по одному, `finish`.
    /// Первая ошибка прекращает приём.
    async fn receive<D: NorFlash, S: NorFlash>(
        flash: &mut Updater<Partitions<D, S>>,
        announced: u32,
        chunks: impl IntoIterator<Item = Vec<u8>>,
    ) -> Received {
        let mut transfer = match Transfer::start(flash, announced).await {
            Ok(transfer) => transfer,
            Err(err) => {
                return Received {
                    result: Err(err),
                    taken: 0,
                };
            }
        };
        let mut taken = 0;
        let mut offset = 0;
        for chunk in chunks {
            taken += 1;
            let at = offset;
            offset += u32::try_from(chunk.len()).unwrap();
            if let Err(err) = transfer.chunk(flash, at, &chunk).await {
                return Received {
                    result: Err(err),
                    taken,
                };
            }
        }
        Received {
            result: transfer.finish(flash).await,
            taken,
        }
    }

    fn image(len: u32, seed: u32) -> Vec<u8> {
        (0..len).map(|i| (i * seed) as u8).collect()
    }

    /// Принимает `data` кусками `chunks` над разделом со словом `W` и
    /// отдаёт принятую длину и содержимое раздела.
    fn reassemble<const W: usize>(data: &[u8], chunks: Vec<Vec<u8>>) -> (u32, Vec<u8>) {
        let mut flash = flash_with_word::<W>(1024);
        let len = tokio::runtime::Builder::new_current_thread()
            .build()
            .expect("рантайм")
            .block_on(receive(&mut flash, data.len() as u32, chunks))
            .result
            .expect("приём");
        (len, flash.memory().clone())
    }

    proptest::proptest! {
        /// Как бы канал ни нарезал образ и какой бы ширины ни было слово флеша,
        /// во флеше оказывается ровно образ, добитый `0xFF` до целого слова.
        /// Три теста ниже проверяют по одной нарезке, здесь их тысячи.
        #[test]
        fn any_chunking_reassembles_the_image(
            data in proptest::collection::vec(proptest::prelude::any::<u8>(), 1..300),
            sizes in proptest::collection::vec(1usize..=40, 1..20),
            word in proptest::sample::select(vec![1u32, 2, 4, 8, 16, 32]),
        ) {
            let mut rest = data.as_slice();
            let mut chunks = Vec::new();
            for size in sizes.iter().cycle() {
                if rest.is_empty() {
                    break;
                }
                let (head, tail) = rest.split_at((*size).min(rest.len()));
                chunks.push(head.to_vec());
                rest = tail;
            }
            // Слово флеша — константа типа раздела: по экземпляру на ширину.
            let (len, memory) = match word {
                1 => reassemble::<1>(&data, chunks),
                2 => reassemble::<2>(&data, chunks),
                4 => reassemble::<4>(&data, chunks),
                8 => reassemble::<8>(&data, chunks),
                16 => reassemble::<16>(&data, chunks),
                _ => reassemble::<32>(&data, chunks),
            };

            proptest::prop_assert_eq!(len, data.len() as u32);
            proptest::prop_assert_eq!(&memory[..data.len()], &data[..]);
            let padded = data.len().next_multiple_of(word as usize);
            proptest::prop_assert!(memory[data.len()..padded].iter().all(|b| *b == 0xFF));
        }
    }

    /// Принять образ кусками по одному байту — самый недружелюбный случай,
    /// какой может выдать канал.
    #[tokio::test]
    async fn reassembles_an_image_from_single_byte_chunks() {
        let image = image(100, 1);
        let chunks = image.iter().map(|b| vec![*b]);
        let mut flash = flash(1024);

        let len = receive(&mut flash, image.len() as u32, chunks)
            .await
            .result
            .expect("приём должен завершиться");

        assert_eq!(len, image.len() as u32);
        assert_eq!(&flash.memory()[..image.len()], &image[..]);
    }

    /// Куски, не кратные ни слову, ни друг другу.
    #[tokio::test]
    async fn reassembles_an_image_from_ragged_chunks() {
        let image = image(200, 7);
        let chunks = image.chunks(13).map(<[u8]>::to_vec);
        let mut flash = flash(1024);

        receive(&mut flash, image.len() as u32, chunks)
            .await
            .result
            .expect("приём");

        assert_eq!(&flash.memory()[..image.len()], &image[..]);
        // Всё, что дошло до флеша, было кратно слову — иначе фейк отказал бы;
        // здесь же проверяется, что запись вообще шла словами, а не одним
        // куском в конце.
        assert!(
            flash.writes().len() > 1,
            "запись должна идти по мере приёма"
        );
    }

    /// Слово в 32 байта (самое широкое у STM32) и образ, не кратный ему.
    #[tokio::test]
    async fn pads_the_last_word_of_an_unaligned_image() {
        let image = image(70, 1);
        let chunks = vec![image.clone()];
        let mut flash = flash_with_word::<MAX_WRITE_SIZE>(1024);

        receive(&mut flash, image.len() as u32, chunks)
            .await
            .result
            .expect("приём");

        assert_eq!(&flash.memory()[..image.len()], &image[..]);
        // Хвост добит единицами — фиксированным значением, а не мусором.
        assert_eq!(&flash.memory()[image.len()..96], &[0xFF; 26][..]);
    }

    /// Нулевая длина отвергается здесь, а не оставляется реализации порта:
    /// `prepare(0)` не стёр бы ничего, и первая же запись легла бы в нестёртую
    /// память, на F2/F4/F7 молча.
    #[tokio::test]
    async fn refuses_an_empty_image_before_touching_the_partition() {
        let chunks = Vec::new();
        let mut flash = flash(128);
        flash.memory().fill(0xA5);

        let refused = receive(&mut flash, 0, chunks).await.result;

        assert_eq!(refused, Err(DownloadError::Empty));
        assert!(!flash.prepared(), "раздел не должен быть подготовлен");
        assert!(
            flash.memory().iter().all(|b| *b == 0xA5),
            "раздел не должен быть стёрт"
        );
    }

    /// Образ длиннее раздела отвергается ДО стирания: иначе устройство
    /// осталось бы и без нового образа, и без того, куда откатываться.
    #[tokio::test]
    async fn refuses_an_image_longer_than_the_partition_without_erasing() {
        let chunks = Vec::new();
        let mut flash = flash(128);
        flash.memory().fill(0xA5);

        let refused = receive(&mut flash, 129, chunks).await.result;

        assert_eq!(
            refused,
            Err(DownloadError::TooLong {
                announced: 129,
                capacity: 128
            })
        );
        assert!(!flash.prepared());
        assert!(flash.memory().iter().all(|b| *b == 0xA5));
    }

    /// Канал прислал больше, чем обещал: приём прекращается на том куске, где
    /// это стало видно, — канал дальше не читается.
    #[tokio::test]
    async fn refuses_more_data_than_announced() {
        let chunks = vec![vec![0; 16], vec![0; 1], vec![0; 100]];
        let mut flash = flash(1024);

        let received = receive(&mut flash, 16, chunks).await;
        let refused = received.result;

        assert_eq!(
            refused,
            Err(DownloadError::TooMuchData {
                announced: 16,
                received: 17
            })
        );
        assert_eq!(received.taken, 2, "после лишнего куска приём прекращается");
    }

    /// Повтор уже принятого куска (ответ на него потерялся) подтверждается
    /// без записи: образ не сдвигается, приём идёт дальше.
    #[tokio::test]
    async fn a_repeated_chunk_is_acknowledged_without_writing() {
        let image = image(16, 5);
        let mut flash = flash(128);
        let mut transfer = Transfer::start(&mut flash, 16).await.unwrap();

        assert_eq!(transfer.chunk(&mut flash, 0, &image[..8]).await, Ok(8));
        let writes = flash.writes().len();
        assert_eq!(
            transfer.chunk(&mut flash, 0, &image[..8]).await,
            Ok(8),
            "повтор"
        );
        assert_eq!(flash.writes().len(), writes, "повтор не пишется");
        assert_eq!(transfer.chunk(&mut flash, 8, &image[8..]).await, Ok(16));
        assert_eq!(transfer.finish(&mut flash).await, Ok(16));
        assert_eq!(&flash.memory()[..16], &image[..]);
    }

    /// Разрыв и наложение — отказ: кусок не встык к принятым.
    #[tokio::test]
    async fn a_gap_or_an_overlap_is_out_of_order() {
        let mut flash = flash(128);
        let mut transfer = Transfer::start(&mut flash, 32).await.unwrap();
        transfer.chunk(&mut flash, 0, &[0; 8]).await.unwrap();

        assert_eq!(
            transfer.chunk(&mut flash, 16, &[0; 8]).await,
            Err(DownloadError::OutOfOrder {
                expected: 8,
                offset: 16
            }),
            "разрыв"
        );
        assert_eq!(
            transfer.chunk(&mut flash, 4, &[0; 8]).await,
            Err(DownloadError::OutOfOrder {
                expected: 8,
                offset: 4
            }),
            "наложение"
        );
    }

    /// Передача оборвалась (канал ответил «конец» раньше времени): делать вид,
    /// что всё в порядке, нельзя.
    #[tokio::test]
    async fn refuses_an_incomplete_transfer() {
        let chunks = vec![vec![0; 20]];
        let mut flash = flash(1024);

        let refused = receive(&mut flash, 32, chunks).await.result;

        assert_eq!(
            refused,
            Err(DownloadError::Incomplete {
                announced: 32,
                received: 20
            })
        );
    }

    /// Отказ флеша доходит до вызывающего как есть, а не теряется.
    #[tokio::test]
    async fn passes_a_flash_failure_through() {
        let chunks = vec![vec![0; 8]];
        let mut flash = flash(1024);
        flash.refuse_writes_from(0);

        let failed = receive(&mut flash, 8, chunks).await.result;

        assert_eq!(
            failed,
            Err(DownloadError::Flash(UpdaterError::Flash(
                NorFlashErrorKind::Other
            )))
        );
    }
}

#[cfg(test)]
mod update_tests {
    use embedded_storage::nor_flash::NorFlashErrorKind;

    use super::{UpdateError, apply};
    use crate::ota::UpdaterError;
    use crate::ota::fake::{LAYOUT, SIGNATURE, flash, with_image};
    use crate::ota::version::pack;

    /// Версия «этой прошивки» в тестах.
    const CURRENT: u32 = pack(1, 2, 3);

    #[tokio::test]
    async fn records_a_newer_signed_image_for_the_bootloader() {
        let mut flash = with_image(64, pack(1, 2, 4));

        apply(&mut flash, CURRENT, &SIGNATURE, 64)
            .await
            .expect("образ новее");

        assert_eq!(flash.recorded(), Some((SIGNATURE, 64)));
        assert!(flash.swap_requested(), "обмен помечен после записи");
    }

    /// Длина — первой, и своим вариантом: пока защиты отвечали одинаково,
    /// удаление одной из них тест не замечал.
    #[tokio::test]
    async fn refuses_a_length_beyond_capacity_before_anything_else() {
        let mut flash = with_image(64, pack(1, 2, 4));
        flash.set_busy(); // и занят — а отказ всё равно по длине

        let refused = apply(&mut flash, CURRENT, &SIGNATURE, 129).await;

        assert_eq!(
            refused,
            Err(UpdateError::TooLong {
                len: 129,
                capacity: 128
            })
        );
        assert!(flash.recorded().is_none());
    }

    /// Занятый раздел — до версии: версия у образа отката заведомо старее, и
    /// ответ «откат» соврал бы о причине.
    #[tokio::test]
    async fn refuses_a_busy_partition_instead_of_calling_it_a_rollback() {
        let mut flash = with_image(64, pack(0, 0, 1));
        flash.set_busy();

        assert_eq!(
            apply(&mut flash, CURRENT, &SIGNATURE, 64).await,
            Err(UpdateError::Busy)
        );
    }

    /// Хвост пишет `cargo xtask build`, а читает `apply`, и общего
    /// кода у них нет — значит разъехаться они могут только молча.
    ///
    /// Ловит это ПАРА тестов: здесь и `writes_the_tail_domain_reads` в
    /// `crates-host/xtask/src/firmware.rs`, где стоят те же литералы.
    /// Поодиночке ни один из них расхождения не увидит — каждый проверяет
    /// свою сторону.
    ///
    /// Версия образа равна текущей, поэтому ответ — откат, и он несёт
    /// прочитанную версию; до него `apply` доходит, только если
    /// раскладка в хвосте совпала с отпечатком фейка.
    #[tokio::test]
    async fn reads_the_tail_xtask_writes() {
        const LAYOUT_LITERAL: u32 = 0x5EED_1A70;
        const VERSION_LITERAL: u32 = 0x0102_0003;
        const TAIL: [u8; 8] = [0x70, 0x1A, 0xED, 0x5E, 0x03, 0x00, 0x02, 0x01];
        assert_eq!(
            LAYOUT, LAYOUT_LITERAL,
            "разделы тестов отдают раскладку из литерала"
        );
        let mut flash = flash(128);
        flash.memory()[56..64].copy_from_slice(&TAIL);

        assert_eq!(
            apply(&mut flash, VERSION_LITERAL, &SIGNATURE, 64).await,
            Err(UpdateError::Rollback {
                current: VERSION_LITERAL,
                incoming: VERSION_LITERAL
            })
        );
    }

    /// Порог — весь хвост (раскладка и версия), а не одна версия.
    #[tokio::test]
    async fn refuses_an_image_shorter_than_its_tail() {
        let mut flash = with_image(64, pack(1, 2, 4));

        assert_eq!(
            apply(&mut flash, CURRENT, &SIGNATURE, 7).await,
            Err(UpdateError::Truncated { len: 7 })
        );
    }

    /// Образ под чужую раскладку — свой отказ, и обмен не помечается; версия
    /// у него новее, так что проверка раскладки — не попутный откат.
    #[tokio::test]
    async fn refuses_an_image_built_for_another_layout() {
        let mut flash = with_image(64, pack(1, 2, 4));
        flash.memory()[56..60].copy_from_slice(&(!LAYOUT).to_le_bytes());

        assert_eq!(
            apply(&mut flash, CURRENT, &SIGNATURE, 64).await,
            Err(UpdateError::Layout {
                device: LAYOUT,
                image: !LAYOUT
            })
        );
        assert!(flash.recorded().is_none());
        assert!(
            !flash.swap_requested(),
            "обмен под чужую раскладку не помечается"
        );
    }

    /// Та же версия — тоже откат: повторная заливка того же номера — типичный
    /// вид атаки.
    #[tokio::test]
    async fn refuses_an_older_or_equal_image() {
        for incoming in [pack(1, 2, 3), pack(1, 1, 9)] {
            let mut flash = with_image(64, incoming);

            assert_eq!(
                apply(&mut flash, CURRENT, &SIGNATURE, 64).await,
                Err(UpdateError::Rollback {
                    current: CURRENT,
                    incoming
                })
            );
            assert!(flash.recorded().is_none());
            assert!(!flash.swap_requested());
        }
    }

    /// Отказ флеша при записи для bootloader'а доходит как отказ флеша, и
    /// обмен без записи не помечается.
    #[tokio::test]
    async fn passes_a_flash_failure_while_recording_through() {
        let mut flash = with_image(64, pack(1, 2, 4));
        flash.refuse_record();

        assert_eq!(
            apply(&mut flash, CURRENT, &SIGNATURE, 64).await,
            Err(UpdateError::Flash(UpdaterError::Flash(
                NorFlashErrorKind::Other
            )))
        );
        assert!(!flash.swap_requested(), "обмен без записи не помечается");
    }
}
