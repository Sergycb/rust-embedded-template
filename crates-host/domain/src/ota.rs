//! Узел `OTA` графа задач: принимает образ по каналу и применяет его — в
//! цикле, пока жив канал.
//!
//! Здесь, а не в `crates-cross/app`, по общему правилу проекта (см.
//! `domain::heartbeat`): что узел делает — логика, и проверяется она на хосте на
//! фейках обоих портов. Проекту остаётся канал — реализация
//! [`ImageSource`](ports::ImageSource) под свой транспорт (USB CDC, UART,
//! сеть) в `bsp`: заголовок, куски, отчёт отправителю — и одна строка в
//! `fragments:` графа.
//!
//! Задача — автомат `#[fsm::typestate]` на контексте `OtaCtx`, который
//! эмитит фрагмент `OTA_FRAG`: заголовок → проверки до стирания → приём →
//! применение → отчёт → снова заголовок; отказ канала — выходное состояние.
//!
//! Применение различается по Cargo-варианту (`signed`), а Liquid в `domain`
//! запрещён, поэтому режим — данные входа узла: [`Mode::Plain`](crate::ota::Mode::Plain) или
//! [`Mode::Signed`](crate::ota::Mode::Signed), и выбирает его `crates-cross/app/src/graph.rs`.
//!
//! Что узел делает с исходом, он не решает:
//! [`ImageSource::finish`](ports::ImageSource::finish) получает `Ok(())` или
//! код отказа [`Rejection`](ports::Rejection), а ответить хосту, сбросить МК
//! или и то и другое — выбор реализации порта. Подробности — `docs/ota.md`.

/// Приём образа по кускам: сверка длины, стирание один раз, буферизация до
/// слова флеша. Функция над портами `ImageSource` и `FirmwareUpdate` — см.
/// модуль.
pub mod download;

/// Применение подписанного обновления: порядок проверок, в котором каждый
/// отказ различим, и запись подписи для bootloader'а. Функция над портом
/// `FirmwareUpdate` — см. модуль.
pub mod update;

/// Версия прошивки в 32 битах и правило приёма обновления (защита от
/// отката). Образец границы: `bsp` достаёт четыре байта из флеша, `domain`
/// решает, что с ними делать, — см. модуль.
pub mod version;

/// Фейки портов обновления, общие для тестов узла и обоих подмодулей.
#[cfg(test)]
mod fakes;

use core::fmt::Debug;

use defmt_or_log::{Debug2Format, info, warn};
use embassy_time::Duration;
use ports::{Announce, DownloadError, FirmwareUpdate, ImageSource, Rejection, UpdateError};
use supervisor::policy::{BackoffPolicy, JitterPolicy};
use supervisor::runtime::TaskExit;

/// Политика пауз между перезапусками узла — те же цифры, что у
/// [`heartbeat::BACKOFF`](crate::heartbeat::BACKOFF), и своя константа
/// намеренно: фрагмент самодостаточен, а у канала доставки свои поводы
/// разойтись с холостым циклом (например, дать хосту время переподключиться).
pub const BACKOFF: BackoffPolicy = BackoffPolicy {
    first: Duration::from_millis(50),
    factor: 2,
    jitter: JitterPolicy::None,
    floor: Duration::from_millis(50),
    max: Duration::from_secs(5),
};

// Вход узла — `OtaWiring`: структуру эмитит сам фрагмент (`boot: …
// struct`). Строит её compose-site из поля платы `Board::ota`
// (`bsp` про `domain` не знает, поэтому раскладка там), подставляя типы порта
// и адаптера аргументами фрагмента:
//
// ```ignore
// fragments: [::domain::OTA_FRAG<bsp::ota::Link, bsp::ota::Updater> as OTA
//                 = ::domain::ota::OtaWiring { link: board.ota.link, flash: board.ota.flash,
//                                           mode: ::domain::ota::Mode::Plain }];
// ```
//
// Тип объявлен здесь, а не назван графом, по правилу фрагментов
// `supervisor`: подсистема говорит, что ей нужно, приложение это
// конструирует — зависимость направлена от приложения к подсистеме и никогда
// обратно.

/// Чем режимы отличаются: проверка заголовка до стирания и применение.
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
    /// Отличие до приёма одно: заголовок без подписи отвергается ДО стирания
    /// раздела — применить такой образ всё равно нечем, а стирание уничтожило
    /// бы образ, в который устройство откатывается.
    Signed {
        /// Версия этой прошивки (`FW_VERSION` приложения) — защита от отката.
        running_version: u32,
    },
}

impl Mode {
    /// Проверка заголовка ДО стирания раздела.
    fn check(self, announce: &Announce) -> Result<(), Rejection> {
        match self {
            Self::Plain => Ok(()),
            Self::Signed { .. } if announce.signature.is_some() => Ok(()),
            Self::Signed { .. } => Err(Rejection::Signature),
        }
    }

    /// Применение принятого образа длиной `len`.
    async fn apply<F>(self, flash: &mut F, announce: &Announce, len: u32) -> Result<(), Rejection>
    where
        F: FirmwareUpdate,
        F::Error: Debug,
    {
        match self {
            // Без подписи применение — просьба об обмене разделов; проверок у
            // неё нет намеренно (см. `FirmwareUpdate::mark_updated`).
            Self::Plain => flash.mark_updated().await.map_err(|err| {
                warn!("ota: пометить обмен не удалось: {:?}", Debug2Format(&err));
                Rejection::Device
            }),
            Self::Signed { running_version } => {
                // `check` уже отказал бы без подписи; ветка `None` здесь —
                // чтобы не паниковать, а не потому что она достижима.
                let Some(signature) = announce.signature.as_ref() else {
                    return Err(Rejection::Signature);
                };
                update::apply_signed(flash, running_version, signature, len)
                    .await
                    .map_err(|err| {
                        warn!("ota: применение отвергнуто: {:?}", Debug2Format(&err));
                        update_rejection(&err)
                    })
            }
        }
    }
}

/// Узел `OTA`: принимает и применяет образы, пока жив канал.
///
/// После каждого цикла — удачного или нет — ждёт следующий заголовок.
/// `TaskExit::Failed` — только когда отказал сам канал (`begin`, `next` или
/// `finish`): автомат уходит в выходное состояние `Down`, граф перезапустит
/// узел с backoff'ом, и объект канала вернётся в слот к новому прогону.
/// `Completed` не возвращается никогда.
///
/// Без `watchdog:` намеренно — по тому же правилу, что у
/// [`heartbeat::run`](crate::heartbeat::run): отметка ставится после
/// реального прогресса, а пока узел ждёт заголовок в `begin()`, прогресса у
/// него нет, и отметка по таймеру сторожила бы исполнитель, а не работу.
/// Аппаратный сторож кормит тикер графа, а не какой-то узел.
pub async fn run<S, F>(mut ctx: OtaCtx<'_, S, F>) -> TaskExit
where
    S: ImageSource,
    S::Error: Debug,
    F: FirmwareUpdate,
    F::Error: Debug,
{
    ctx.run().await.result
}

#[fsm::typestate]
impl<S, F> OtaCtx<'_, S, F>
where
    S: ImageSource,
    S::Error: Debug,
    F: FirmwareUpdate,
    F::Error: Debug,
{
    fsm::machine! {
        machine Ota mod edges {
            states {
                /// Ждёт заголовок следующего образа.
                Waiting,
                /// Заголовок прошёл проверки до стирания — образ принимается.
                Accepted { announce: Announce },
                /// Образ принят целиком, `len` байт.
                Received { announce: Announce, len: u32 },
                /// Исход для отправителя.
                Verdict { outcome: Result<(), Rejection> },
                /// Отказал сам канал: сообщать некому, прогон окончен.
                exit Down { result: TaskExit },
            }
            start => begin -> Waiting;
            listen {
                Waiting => announce: self.link.begin(),
            }
            transitions {
                Waiting + announce(Ok(a))  => screen  -> Accepted | Verdict,
                Waiting + announce(Err(e)) => lost    -> Down,
                Accepted                   => receive -> Received | Verdict | Down,
                Received                   => apply   -> Verdict,
                Verdict                    => report  -> Waiting | Down,
            }
        }
    }

    /// Подтверждение образа, затем ожидание заголовка.
    ///
    /// Без подтверждения bootloader считает попытку неудачной и на следующем
    /// сбросе откатывает образ — обновление отработало бы ровно один запуск.
    /// Здесь, а не в `main` до исполнителя: порт асинхронный, и драйвер внешней
    /// флеши может ждать таймер или прерывание, а это требует исполнителя. На
    /// обычной загрузке вызов ничего не меняет (состояние и так `Boot`), на
    /// рестарте узла — тоже.
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
    /// Один раз за запуск, а не на каждом рестарте узла (`confirmed` — слот,
    /// он рестарт переживает): `STATE` после обмена и `STATE` с принятым, но
    /// ещё не применённым образом выглядят одинаково (`Swap`), и подтверждение
    /// на рестарте — после отказа канала уже за `mark_updated` — молча сняло бы
    /// запрос на обмен, о котором отправитель уже услышал `Ok`. Не удалось —
    /// флаг не ставится, и следующий рестарт попробует снова.
    async fn begin(&mut self) -> Waiting {
        info!("domain: узел OTA запущен");
        if !*self.confirmed {
            match self.flash.mark_booted().await {
                Ok(()) => *self.confirmed = true,
                Err(err) => warn!(
                    "ota: не удалось подтвердить образ: {:?}",
                    Debug2Format(&err)
                ),
            }
        }
        Waiting
    }

    /// Проверки до стирания: занятость, затем режим (подпись).
    async fn screen(&mut self, _: Waiting, announce: Announce) -> edges::Screen {
        info!("ota: заголовок: {} байт", announce.len);
        // Занятость — до всего: адаптер и так откажет в `prepare`, но тогда
        // отправитель услышал бы `Device` вместо «сначала перезагрузи».
        let outcome = match self.flash.is_busy().await {
            Ok(false) => self.mode.check(&announce),
            Ok(true) => Err(Rejection::Busy),
            Err(err) => {
                warn!("ota: флеш не ответил о занятости: {:?}", Debug2Format(&err));
                Err(Rejection::Device)
            }
        };
        match outcome {
            Ok(()) => Accepted { announce }.into(),
            Err(rejection) => Verdict {
                outcome: Err(rejection),
            }
            .into(),
        }
    }

    /// Приём образа; длину сверяет `receive` до `prepare`.
    async fn receive(&mut self, s: Accepted) -> edges::Receive {
        match download::receive(self.link, self.flash, s.announce.len).await {
            Ok(len) => Received {
                announce: s.announce,
                len,
            }
            .into(),
            // Канал — без warn! о приёме: о нём скажет `lost`, сообщать
            // отправителю некому.
            Err(DownloadError::Source(err)) => self.lost(err).into(),
            Err(err) => {
                warn!("ota: приём не удался: {:?}", Debug2Format(&err));
                match download_rejection(err) {
                    Ok(rejection) => Verdict {
                        outcome: Err(rejection),
                    }
                    .into(),
                    Err(err) => self.lost(err).into(),
                }
            }
        }
    }

    /// Применяет принятый образ через режим (plain или signed).
    async fn apply(&mut self, s: &Received) -> Verdict {
        Verdict {
            outcome: self.mode.apply(self.flash, &s.announce, s.len).await,
        }
    }

    /// Исход — в лог и отправителю.
    async fn report(&mut self, s: Verdict) -> edges::Report {
        match s.outcome {
            Ok(()) => info!("ota: образ принят, обмен разделов на следующем сбросе"),
            Err(rejection) => warn!("ota: образ отвергнут: {:?}", rejection),
        }
        match self.link.finish(s.outcome).await {
            Ok(()) => Waiting.into(),
            Err(err) => self.lost(err).into(),
        }
    }

    /// Отказ канала: сообщать отправителю некому, прогон кончается `Failed`.
    #[expect(
        clippy::unused_self,
        reason = "сигнатуру действия задаёт таблица `#[fsm::typestate]`"
    )]
    fn lost(&mut self, err: S::Error) -> Down {
        warn!("ota: отказ канала: {:?}", Debug2Format(&err));
        Down {
            result: TaskExit::Failed,
        }
    }
}

/// Код отказа для отправителя; `Err` — отказал сам канал, и сообщать некому.
///
/// `receive` перехватывает `Source` раньше — здесь эта ветка нужна
/// тотальности, а не потоку.
fn download_rejection<S, F>(err: DownloadError<S, F>) -> Result<Rejection, S> {
    Ok(match err {
        DownloadError::Empty | DownloadError::TooLong { .. } => Rejection::Length,
        DownloadError::TooMuchData { .. } | DownloadError::Incomplete { .. } => Rejection::Transfer,
        DownloadError::Source(err) => return Err(err),
        // `UnusableGranularity`, `Flash` и незнакомые варианты (перечисление
        // `non_exhaustive`) сообщаются как отказ устройства, а не прячутся.
        _ => Rejection::Device,
    })
}

/// Код отказа для отправителя по ошибке применения с подписью.
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
// (`OtaWiring::mode`), а не второй фрагмент, иначе было бы два контекста и две
// одинаковые таблицы автомата. Какой режим подставить — решает `graph.rs`.
//
// `boot: inputs: …` — собственная привязка фрагмента: compose-site пишет
// `fragments: [::domain::OTA_FRAG<…> as OTA = ::domain::ota::OtaWiring { link: …, flash: …, mode: … }]`,
// а `spawn_all` связывает её первой строкой пролога, и инициализаторы слотов
// ниже читают её поля. Так фрагмент не видит `Board` вовсе — только то, что
// ему дали. `MODE` — обычный слот, не `local`: перечисление из данных
// `Send` всегда. `CONFIRMED` — без входа: подтверждён ли уже образ в этом
// запуске (см. действие `begin`); слот, а не поле автомата, потому что должен
// пережить рестарт узла.
//
// `ctx:` — контекст [`OtaCtx`] эмитит фрагмент здесь, в `domain`; автомат
// стоит на нём (см. `domain::heartbeat`).
//
// `<S, F>` — параметры фрагмента: тип ресурсного слота — `static`, назвать
// его фрагмент обязан, а конкретный тип (`bsp::ota::Updater`, транспорт проекта)
// `domain` не видит и видеть не должен. Compose-site подставляет их в
// `fragments:` угловыми скобками, по позиции; bound'ов у параметров нет
// намеренно — их держит сигнатура `run`, и rustc проверяет её, а не копию в
// DSL. Всё остальное, что фрагмент называет, написано полным путём
// (`$crate::ota::BACKOFF`, `::supervisor::…`) — от `graph.rs` он не требует
// ни одного объявления, см. `HEARTBEAT_FRAG`.
//
// `local` на обоих слотах: объекты платы — `!Send` (`bsp::ota::Updater` держит
// `&'static Mutex<NoopRawMutex, …>`, а `NoopRawMutex` намеренно не `Sync`),
// тогда как обычный слот — `static`, которому нужен `Send`. `local` меняет
// слот на `LocalResourceSlot` и снимает это требование, взамен обязывая
// держать значение на одном исполнителе — том, что спавнит граф. Это то же
// допущение, на котором держится `NoopRawMutex` у `FlashMutex`: один
// исполнитель, без вытеснения. `LINK` помечен тоже, хотя заглушка шаблона
// `Send`: транспорт проекта вправе быть `!Send` (DMA-буферы, периферия), и
// править ради этого фрагмент здесь не придётся. Compose-site платит фичей
// `supervisor/local-resources` и `unsafe`-блоком на каждый `local`-`static`
// (здесь два), которые макрос эмитит в `app`. Узел, чей `local`-слот
// заполняется инициализатором (`= inputs.…`, как здесь), нельзя перевести на
// другой исполнитель (`executor:`): значение строится в прологе `spawn_all`
// на исполнителе графа, а забирать его с другого запрещено; `local` без
// инициализатора такого ограничения не несёт.
//
// Без `watchdog:` намеренно — см. `run`.
supervisor::supervisor_fragment! {
    name: OTA_FRAG<S, F>;
    boot: inputs: struct $crate::ota::OtaWiring;

    node deps: [], restart: ::supervisor::policy::RestartPolicy::OnFailure,
        backoff: $crate::ota::BACKOFF,
        resources: [
            /// Канал доставки образа — реализация `ImageSource` из `bsp`.
            LINK: local S = inputs.link,
            /// Адаптер обновления — реализация `FirmwareUpdate` поверх разделов флеша.
            FLASH: local F = inputs.flash,
            /// Режим применения: `Mode::Plain` или `Mode::Signed`.
            MODE: $crate::ota::Mode = inputs.mode,
            CONFIRMED: bool = false
        ],
        ctx: $crate::ota::OtaCtx,
        task: $crate::ota::run;
}

#[cfg(test)]
mod tests {
    use super::{Mode, OtaCtx, run};
    use crate::ota::fakes::{FakeFlash, FakeLink, SIGNATURE};
    use crate::ota::update::VERSION_BYTES;
    use crate::ota::version::pack;
    use ports::{Announce, Rejection};
    use supervisor::runtime::TaskExit;

    /// Гоняет узел, пока не кончится канал: после последнего заголовка `begin`
    /// у `FakeLink` отвечает отказом, так что прогон всегда кончается `Failed`
    /// — раньше, если канал отказал посреди цикла.
    async fn serve(link: &mut FakeLink, flash: &mut FakeFlash, mode: Mode) {
        serve_again(link, flash, mode, &mut false).await;
    }

    /// То же, но со слотом `CONFIRMED` снаружи — как у узла, перезапущенного
    /// графом.
    async fn serve_again(
        link: &mut FakeLink,
        flash: &mut FakeFlash,
        mut mode: Mode,
        confirmed: &mut bool,
    ) {
        let ctx = OtaCtx {
            link,
            flash,
            mode: &mut mode,
            confirmed,
            index: 0,
        };
        assert_eq!(run(ctx).await, TaskExit::Failed);
    }

    fn plain() -> Mode {
        Mode::Plain
    }

    #[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
    fn announce(len: usize) -> Announce {
        Announce {
            len: len as u32,
            signature: None,
        }
    }

    #[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
    fn image(len: usize) -> Vec<u8> {
        (0..len).map(|i| (i * 3) as u8).collect()
    }

    /// Счастливый путь: раздел подготовлен один раз, байты на месте, обмен
    /// запрошен, отправитель получил `Ok`.
    #[tokio::test]
    async fn applies_an_unsigned_image_and_reports_success() {
        let image = image(64);
        let mut link =
            FakeLink::of(image.chunks(13).map(<[u8]>::to_vec)).announcing([announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);

        serve(&mut link, &mut flash, plain()).await;

        assert_eq!(flash.prepared, Some(64));
        assert_eq!(&flash.memory[..64], &image[..]);
        assert_eq!(flash.updated, 1, "обмен разделов запрошен один раз");
        assert_eq!(link.finished, vec![Ok(())]);
    }

    /// Образ подтверждается при старте узла — до первого заголовка, даже если
    /// канал отказал сразу: иначе bootloader откатил бы обновление на
    /// следующем сбросе.
    #[tokio::test]
    async fn confirms_the_running_image_before_the_first_header() {
        let mut link = FakeLink::of([]);
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, plain()).await;

        assert_eq!(flash.booted, 1);
        assert_eq!(flash.prepared, None, "раздел не тронут");
    }

    /// Рестарт узла после принятого образа (канал отказал уже после
    /// `mark_updated`) не подтверждает снова: иначе запрос на обмен, о котором
    /// отправитель услышал `Ok`, был бы молча снят.
    #[tokio::test]
    async fn a_restarted_node_does_not_confirm_again() {
        let image = image(16);
        let mut confirmed = false;
        let mut flash = FakeFlash::new(64, 8);

        let mut link = FakeLink::of([image]).announcing([announce(16)]);
        serve_again(&mut link, &mut flash, plain(), &mut confirmed).await;
        assert_eq!((flash.booted, flash.updated), (1, 1));

        let mut link = FakeLink::of([]);
        serve_again(&mut link, &mut flash, plain(), &mut confirmed).await;
        assert_eq!(flash.booted, 1, "второй прогон не подтверждает");
    }

    /// Подтверждение не удалось — узел работает дальше и пробует снова на
    /// следующем рестарте.
    #[tokio::test]
    async fn a_failed_confirmation_is_retried_on_restart() {
        let mut confirmed = false;
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_booted = Err("флеш не отвечает");

        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        serve_again(&mut link, &mut flash, plain(), &mut confirmed).await;
        assert!(!confirmed);
        assert_eq!(
            link.finished,
            vec![Ok(())],
            "образ принят, несмотря на отказ"
        );

        flash.mark_booted = Ok(());
        let mut link = FakeLink::of([]);
        serve_again(&mut link, &mut flash, plain(), &mut confirmed).await;
        assert!(confirmed);
    }

    /// Занятость — до стирания и до чтения канала: адаптер и так отказал бы
    /// в `prepare`, но отправитель услышал бы `Device` вместо «перезагрузи».
    #[tokio::test]
    async fn reports_busy_before_touching_the_partition_or_the_link() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);
        flash.busy = true;

        serve(&mut link, &mut flash, plain()).await;

        assert_eq!(link.finished, vec![Err(Rejection::Busy)]);
        assert_eq!(flash.prepared, None, "раздел не должен быть стёрт");
        assert_eq!(link.next, 0, "куски не читались");
    }

    #[tokio::test]
    async fn maps_a_bad_length_to_length_without_erasing() {
        let mut link = FakeLink::of([]).announcing([announce(65)]);
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, plain()).await;

        assert_eq!(link.finished, vec![Err(Rejection::Length)]);
        assert_eq!(flash.prepared, None);
    }

    #[tokio::test]
    async fn maps_excess_data_to_transfer() {
        let mut link = FakeLink::of([vec![0; 16], vec![0; 8]]).announcing([announce(16)]);
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, plain()).await;

        assert_eq!(link.finished, vec![Err(Rejection::Transfer)]);
        assert_eq!(flash.updated, 0, "обмен не запрашивался");
    }

    /// Негодная гранулярность — вина устройства, не отправителя, и до
    /// стирания: `receive` отказывает, не трогая раздел.
    #[tokio::test]
    async fn maps_an_unusable_granularity_to_device_without_erasing() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);
        flash.word = 0;

        serve(&mut link, &mut flash, plain()).await;

        assert_eq!(link.finished, vec![Err(Rejection::Device)]);
        assert_eq!(flash.prepared, None);
    }

    #[tokio::test]
    async fn maps_a_flash_failure_on_apply_to_device() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_updated = Err("флеш отказал");

        serve(&mut link, &mut flash, plain()).await;

        assert_eq!(link.finished, vec![Err(Rejection::Device)]);
    }

    /// Обрыв канала посреди приёма: сообщать некому, прогон кончается
    /// `Failed`, `finish` не зовётся. Второй заголовок — проверка, что узел
    /// встал именно на обрыве: продолжи он цикл, его отчёт попал бы в `finished`.
    #[tokio::test]
    async fn a_link_failure_while_receiving_ends_the_run_without_a_report() {
        let mut link =
            FakeLink::of([vec![0; 8], vec![0; 8]]).announcing([announce(16), announce(8)]);
        link.fail_at = Some(1);
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, plain()).await;

        assert!(link.finished.is_empty(), "сообщать некому");
    }

    /// Обрыв при отчёте кончает прогон: второй заголовок уже не читается.
    #[tokio::test]
    async fn a_link_failure_on_finish_ends_the_run() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8), announce(8)]);
        link.finish = Err("обрыв при отчёте");
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, plain()).await;

        assert_eq!(link.finished, vec![Ok(())], "второго цикла не было");
    }

    /// После отчёта автомат возвращается к заголовку: второй цикл идёт
    /// своим ходом — здесь кусков на него не осталось, и он отвергнут.
    #[tokio::test]
    async fn serves_cycle_after_cycle_until_the_link_fails() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8), announce(8)]);
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, plain()).await;

        assert_eq!(link.finished, vec![Ok(()), Err(Rejection::Transfer)]);
    }

    /// С подписью; версия «этой прошивки» — 1.2.3.
    fn signed() -> Mode {
        Mode::Signed {
            running_version: pack(1, 2, 3),
        }
    }

    #[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
    fn signed_announce(len: usize) -> Announce {
        Announce {
            len: len as u32,
            signature: Some(SIGNATURE),
        }
    }

    /// Образ с версией в хвосте — как его собирает `cargo xtask build`.
    fn versioned_image(len: usize, version: u32) -> Vec<u8> {
        let mut image = image(len);
        image[len - VERSION_BYTES as usize..].copy_from_slice(&version.to_le_bytes());
        image
    }

    /// Счастливый путь с подписью: подпись из заголовка и длина доехали до
    /// адаптера записью для bootloader'а, и только после неё помечен обмен.
    #[tokio::test]
    async fn applies_a_signed_image_with_the_signature_from_the_announce() {
        let image = versioned_image(64, pack(1, 2, 4));
        let mut link = FakeLink::of([image.clone()]).announcing([signed_announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);

        serve(&mut link, &mut flash, signed()).await;

        assert_eq!(&flash.memory[..64], &image[..]);
        assert_eq!(flash.recorded, vec![(SIGNATURE, 64)]);
        assert_eq!(flash.updated, 1);
        assert_eq!(link.finished, vec![Ok(())]);
    }

    /// Без подписи в заголовке — отказ ДО стирания: стирание уничтожило бы
    /// образ, в который устройство откатывается, а применить всё равно нечем.
    #[tokio::test]
    async fn refuses_a_signed_update_without_a_signature_before_erasing() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, signed()).await;

        assert_eq!(link.finished, vec![Err(Rejection::Signature)]);
        assert_eq!(flash.prepared, None, "раздел не должен быть стёрт");
        assert_eq!(link.next, 0, "куски не читались");
    }

    /// Та же версия — откат, и код для отправителя свой.
    #[tokio::test]
    async fn maps_a_rollback_to_rollback() {
        let image = versioned_image(64, pack(1, 2, 3));
        let mut link = FakeLink::of([image]).announcing([signed_announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);

        serve(&mut link, &mut flash, signed()).await;

        assert_eq!(link.finished, vec![Err(Rejection::Rollback)]);
        assert!(flash.recorded.is_empty(), "обмен не должен быть запрошен");
    }
}
