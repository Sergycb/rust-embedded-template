//! Узел `OTA` графа задач: принимает образ по каналу и применяет его — в
//! цикле, пока жив канал.
//!
//! Здесь, а не в `crates-cross/app`, по общему правилу проекта (см.
//! `domain::app`): что узел делает — логика, и проверяется она на хосте на
//! фейках обоих портов. Проекту остаётся канал — реализация
//! [`ImageSource`](ports::ImageSource) под свой транспорт (USB CDC, UART,
//! сеть) в `bsp`: заголовок, куски, отчёт отправителю — и одна строка в
//! `fragments:` графа.
//!
//! Задача — автомат `#[fsm::typestate]` на контексте [`OtaCtx`], который
//! эмитит фрагмент `OTA_FRAG`: заголовок → проверки до стирания → приём →
//! применение → отчёт → снова заголовок; отказ канала — выходное состояние.
//!
//! Применение различается по Cargo-варианту (`signed`), а Liquid в `domain`
//! запрещён, поэтому режим — данные входа узла: [`Mode::plain`] или
//! [`Mode::signed`], и выбирает его `crates-cross/app/src/graph.rs`. Обе
//! функции режима лежат в любом проекте; во flash попадает только выбранная —
//! generic без вызова не мономорфизируется.
//!
//! Что узел делает с исходом, он не решает:
//! [`ImageSource::finish`](ports::ImageSource::finish) получает `Ok(())` или
//! код отказа [`Rejection`](ports::Rejection), а ответить хосту, сбросить МК
//! или и то и другое — выбор реализации порта. Подробности — `docs/ota.md`.

use core::fmt::Debug;

use defmt_or_log::{Debug2Format, info, warn};
use embassy_time::Duration;
use ports::{
    Announce, DownloadError, FirmwareUpdate, ImageSource, Rejection, SignedFirmwareUpdate,
    UpdateError,
};
use supervisor::policy::{BackoffPolicy, JitterPolicy};
use supervisor::runtime::TaskExit;

use crate::{download, update};

/// Политика пауз между перезапусками узла — те же цифры, что у
/// [`app::BACKOFF`](crate::app::BACKOFF), и своя константа намеренно: фрагмент
/// самодостаточен, а у канала доставки свои поводы разойтись с холостым циклом
/// (например, дать хосту время переподключиться).
pub const BACKOFF: BackoffPolicy = BackoffPolicy {
    first: Duration::from_millis(50),
    factor: 2,
    jitter: JitterPolicy::None,
    floor: Duration::from_millis(50),
    max: Duration::from_secs(5),
};

/// Что узлу нужно на входе. Строит compose-site из поля платы `Board::ota`
/// (`bsp` про `domain` не знает, поэтому раскладка там), подставляя типы порта
/// и адаптера аргументами фрагмента:
///
/// ```ignore
/// fragments: [::domain::OTA_FRAG<bsp::ota::Link, bsp::ota::Updater> as OTA
///                 = ::domain::ota::Inputs { link: board.ota.link, flash: board.ota.flash,
///                                           mode: ::domain::ota::Mode::plain() }];
/// ```
///
/// Тип объявлен здесь, а не назван графом, по правилу фрагментов
/// `supervisor`: подсистема говорит, что ей нужно, приложение это
/// конструирует — зависимость направлена от приложения к подсистеме и никогда
/// обратно.
#[derive(Debug)]
pub struct Inputs<S, F> {
    /// Канал доставки образа — реализация `ImageSource` из `bsp`.
    pub link: S,
    /// Адаптер обновления — реализация `FirmwareUpdate` поверх разделов флеша.
    pub flash: F,
    /// Режим применения: [`Mode::plain`] или [`Mode::signed`].
    pub mode: Mode<F>,
}

/// Чем режимы отличаются: проверка заголовка до стирания и применение.
///
/// Указатели на функции, а не трейт: две пары по нескольку строк не стоят
/// публичной абстракции, а автомат при этом один — и тестируется один.
#[derive(Debug)]
pub struct Mode<F> {
    /// Проверка заголовка ДО стирания раздела.
    check: fn(&Announce) -> Result<(), Rejection>,
    /// Применение принятого образа длиной `len`.
    apply: fn(&mut F, &Announce, u32) -> Result<(), Rejection>,
}

impl<F> Mode<F>
where
    F: FirmwareUpdate,
    F::Error: Debug,
{
    /// Без проверки подписи: принятый образ применяется
    /// [`FirmwareUpdate::mark_updated`].
    pub fn plain() -> Self {
        Self {
            check: plain_check,
            apply: plain_apply,
        }
    }
}

impl<F> Mode<F>
where
    F: SignedFirmwareUpdate,
    F::Error: Debug,
{
    /// С проверкой подписи: принятый образ применяется
    /// [`apply_signed`](crate::update::apply_signed) — длина, занятость,
    /// версия, ключ, подпись, в этом порядке.
    ///
    /// Отличие до приёма одно: заголовок без подписи отвергается ДО стирания
    /// раздела — применить такой образ всё равно нечем, а стирание уничтожило
    /// бы образ, в который устройство откатывается.
    pub fn signed() -> Self {
        Self {
            check: signed_check,
            apply: signed_apply,
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
/// Без `watchdog:` намеренно — по той же логике, что у [`app::run`](crate::app::run):
/// отметка ставится после реального прогресса, а пока узел ждёт заголовок в
/// `begin()`, прогресса у него нет, и отметка по таймеру сторожила бы
/// исполнитель, а не работу. Железо кормит узел `APP`.
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

    fn begin(&mut self) -> Waiting {
        info!("domain: узел OTA запущен");
        Waiting
    }

    /// Проверки до стирания: занятость, затем режим (подпись).
    fn screen(&mut self, _: Waiting, announce: Announce) -> edges::Screen {
        info!("ota: заголовок: {} байт", announce.len);
        // Занятость — до всего: адаптер и так откажет в `prepare`, но тогда
        // отправитель услышал бы `Device` вместо «сначала перезагрузи».
        let outcome = match self.flash.is_busy() {
            Ok(false) => (self.mode.check)(&announce),
            Ok(true) => Err(Rejection::Busy),
            Err(err) => {
                warn!("ota: флеш не ответил о занятости: {:?}", Debug2Format(&err));
                Err(Rejection::Device)
            }
        };
        match outcome {
            Ok(()) => Accepted { announce }.into(),
            Err(_) => Verdict { outcome }.into(),
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

    fn apply(&mut self, s: Received) -> Verdict {
        Verdict {
            outcome: (self.mode.apply)(self.flash, &s.announce, s.len),
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
    fn lost(&mut self, err: S::Error) -> Down {
        warn!("ota: отказ канала: {:?}", Debug2Format(&err));
        Down {
            result: TaskExit::Failed,
        }
    }
}

/// Без подписи заголовок проверять нечего — длину сверит `receive`.
fn plain_check(_: &Announce) -> Result<(), Rejection> {
    Ok(())
}

/// Без подписи применение — просьба об обмене разделов; проверок у неё нет
/// намеренно (см. [`FirmwareUpdate::mark_updated`]).
fn plain_apply<F>(flash: &mut F, _: &Announce, _: u32) -> Result<(), Rejection>
where
    F: FirmwareUpdate,
    F::Error: Debug,
{
    flash.mark_updated().map_err(|err| {
        warn!("ota: пометить обмен не удалось: {:?}", Debug2Format(&err));
        Rejection::Device
    })
}

/// С подписью заголовок обязан её нести — иначе отказ до стирания.
fn signed_check(announce: &Announce) -> Result<(), Rejection> {
    if announce.signature.is_none() {
        return Err(Rejection::Signature);
    }
    Ok(())
}

/// С подписью применение — [`apply_signed`](crate::update::apply_signed)
/// с подписью из заголовка.
fn signed_apply<F>(flash: &mut F, announce: &Announce, len: u32) -> Result<(), Rejection>
where
    F: SignedFirmwareUpdate,
    F::Error: Debug,
{
    // `signed_check` уже отказал бы без подписи; ветка `None` здесь — чтобы
    // не паниковать, а не потому что она достижима.
    let Some(signature) = announce.signature.as_ref() else {
        return Err(Rejection::Signature);
    };
    update::apply_signed(flash, signature, len).map_err(|err| {
        warn!("ota: применение отвергнуто: {:?}", Debug2Format(&err));
        update_rejection(&err)
    })
}

/// Код отказа для отправителя; `Err` — отказал сам канал, и сообщать некому.
///
/// `receive` перехватывает `Source` раньше — здесь эта ветка нужна
/// тотальности, а не потоку.
fn download_rejection<S, F>(err: DownloadError<S, F>) -> Result<Rejection, S> {
    Ok(match err {
        DownloadError::Empty | DownloadError::TooLong { .. } => Rejection::Length,
        DownloadError::TooMuchData { .. } | DownloadError::Incomplete { .. } => Rejection::Transfer,
        DownloadError::UnusableGranularity(_) | DownloadError::Flash(_) => Rejection::Device,
        DownloadError::Source(err) => return Err(err),
    })
}

/// Код отказа для отправителя по ошибке применения с подписью.
fn update_rejection<E>(err: &UpdateError<E>) -> Rejection {
    match err {
        UpdateError::TooLong { .. } | UpdateError::Truncated { .. } => Rejection::Length,
        UpdateError::Busy => Rejection::Busy,
        UpdateError::Rollback { .. } => Rejection::Rollback,
        UpdateError::NoPublicKey | UpdateError::BadSignature => Rejection::Signature,
        UpdateError::Flash(_) => Rejection::Device,
    }
}

// Узел объявлен здесь же, где лежит его задача, — как `APP_FRAG` в
// `domain::app`; compose-site (`crates-cross/app/src/graph.rs`) только
// перечисляет фрагмент, называет узел (`as OTA`) и кормит его входом. Фрагмент
// один на оба Cargo-варианта `signed`: режим применения — данные входа
// (`Inputs::mode`), а не второй фрагмент, иначе было бы два контекста и две
// одинаковые таблицы автомата. Какой режим подставить — решает `graph.rs`.
//
// `boot: inputs: …` — собственная привязка фрагмента: compose-site пишет
// `fragments: [::domain::OTA_FRAG<…> as OTA = ::domain::ota::Inputs { link: …, flash: …, mode: … }]`,
// а `spawn_all` связывает её первой строкой пролога, и инициализаторы слотов
// ниже читают её поля. Так фрагмент не видит `Board` вовсе — только то, что
// ему дали. `MODE` — обычный слот, не `local`: указатели на функции `Send`
// при любом `F`.
//
// `ctx:` — контекст [`OtaCtx`] эмитит фрагмент здесь, в `domain`; автомат
// стоит на нём (см. `domain::app`).
//
// `<S, F>` — параметры фрагмента: тип ресурсного слота — `static`, назвать
// его фрагмент обязан, а конкретный тип (`bsp::ota::Updater`, транспорт проекта)
// `domain` не видит и видеть не должен. Compose-site подставляет их в
// `fragments:` угловыми скобками, по позиции; bound'ов у параметров нет
// намеренно — их держит сигнатура `run`, и rustc проверяет её, а не копию в
// DSL. Всё остальное, что фрагмент называет, написано полным путём
// (`$crate::ota::BACKOFF`, `::supervisor::…`) — от `graph.rs` он не требует
// ни одного объявления, см. `APP_FRAG`.
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
    boot: inputs: $crate::ota::Inputs<S, F>;

    node deps: [], restart: ::supervisor::policy::RestartPolicy::OnFailure,
        backoff: $crate::ota::BACKOFF,
        resources: [
            LINK: local S = inputs.link,
            FLASH: local F = inputs.flash,
            MODE: $crate::ota::Mode<F> = inputs.mode
        ],
        ctx: $crate::ota::OtaCtx,
        task: $crate::ota::run;
}

#[cfg(test)]
mod tests {
    use super::{Mode, OtaCtx, run};
    use crate::firmware::pack;
    use crate::test_support::{FakeFlash, FakeLink, SIGNATURE};
    use crate::update::VERSION_BYTES;
    use ports::{Announce, Rejection, VerifyError};
    use supervisor::runtime::TaskExit;

    /// Гоняет узел, пока не кончится канал: после последнего заголовка `begin`
    /// у `FakeLink` отвечает отказом, так что прогон всегда кончается `Failed`
    /// — раньше, если канал отказал посреди цикла.
    async fn serve(link: &mut FakeLink, flash: &mut FakeFlash, mut mode: Mode<FakeFlash>) {
        let ctx = OtaCtx {
            link,
            flash,
            mode: &mut mode,
            index: 0,
        };
        assert_eq!(run(ctx).await, TaskExit::Failed);
    }

    fn plain() -> Mode<FakeFlash> {
        Mode::plain()
    }

    fn announce(len: usize) -> Announce {
        Announce {
            len: len as u32,
            signature: None,
        }
    }

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

    fn signed() -> Mode<FakeFlash> {
        Mode::signed()
    }

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
    /// адаптера, `mark_updated` не звался — обмен делает только проверка.
    #[tokio::test]
    async fn applies_a_signed_image_with_the_signature_from_the_announce() {
        let image = versioned_image(64, pack(1, 2, 4));
        let mut link = FakeLink::of([image.clone()]).announcing([signed_announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);

        serve(&mut link, &mut flash, signed()).await;

        assert_eq!(&flash.memory[..64], &image[..]);
        assert_eq!(flash.verified, vec![(SIGNATURE, 64)]);
        assert_eq!(flash.updated, 0);
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
        assert!(flash.verified.is_empty(), "до криптографии дойти не должно");
    }

    #[tokio::test]
    async fn maps_a_bad_signature_to_signature() {
        let image = versioned_image(64, pack(1, 2, 4));
        let mut link = FakeLink::of([image]).announcing([signed_announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);
        flash.verify = Err(VerifyError::BadSignature);

        serve(&mut link, &mut flash, signed()).await;

        assert_eq!(link.finished, vec![Err(Rejection::Signature)]);
    }

    #[tokio::test]
    async fn maps_a_zero_key_to_signature() {
        let image = versioned_image(64, pack(1, 2, 4));
        let mut link = FakeLink::of([image]).announcing([signed_announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);
        flash.key = [0; 32];

        serve(&mut link, &mut flash, signed()).await;

        assert_eq!(link.finished, vec![Err(Rejection::Signature)]);
        assert!(flash.verified.is_empty());
    }
}
