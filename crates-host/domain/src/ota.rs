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
//! применение → отчёт → снова заголовок; отказ канала — в лог и снова ожидание
//! заголовка.
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

use defmt_or_log::{FormatOrDebug, info, warn};
use ports::{
    Announce, DownloadError, FirmwareUpdate, ImageSource, OtaDevice, Rejection, UpdateError,
};
use supervisor::runtime::TaskExit;

// Вход узла — `OtaWiring`: структуру эмитит сам фрагмент (`boot: …
// struct`). Строит её compose-site (`app`) из поля платы `Board::ota` — одного
// объекта, в котором канал доставки образа и адаптер обновления (порт
// `OtaDevice`; `bsp` про `domain` не знает), подставляя его тип аргументом
// фрагмента:
//
// ```ignore
// fragments: [::domain::OTA_FRAG<bsp::ota::Ota> as OTA
//                 = ::domain::ota::OtaWiring { device: board.ota,
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
                let signature = announce.signature.as_ref().ok_or(Rejection::Signature)?;
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

/// Узел `OTA`: принимает и применяет образы, пока жив канал.
///
/// После каждого цикла — удачного или нет — ждёт следующий заголовок. Не
/// выходит никогда: отказ самого канала (`begin`, `next` или `finish`) — в
/// лог, и снова ждёт заголовок; переждать отказ — дело `ImageSource`.
///
/// Без `watchdog:` намеренно — по тому же правилу, что у
/// [`heartbeat::task`](crate::heartbeat::task): отметка ставится после
/// реального прогресса, а пока узел ждёт заголовок в `begin()`, прогресса у
/// него нет, и отметка по таймеру сторожила бы исполнитель, а не работу.
/// Аппаратный сторож кормит тикер графа, а не какой-то узел.
pub async fn task<D>(mut ctx: OtaCtx<'_, D>) -> TaskExit
where
    D: OtaDevice,
    <D::Link as ImageSource>::Error: FormatOrDebug,
    <D::Flash as FirmwareUpdate>::Error: FormatOrDebug,
{
    match ctx.run().await {}
}

#[fsm::typestate]
impl<D> OtaCtx<'_, D>
where
    D: OtaDevice,
    <D::Link as ImageSource>::Error: FormatOrDebug,
    <D::Flash as FirmwareUpdate>::Error: FormatOrDebug,
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
            }
            start => begin -> Waiting;
            listen {
                Waiting => announce: self.device.parts().0.begin(),
            }
            transitions {
                Waiting + announce(Ok(a))  => screen  -> Accepted | Verdict,
                Waiting + announce(Err(e)) => lost    -> Waiting,
                Accepted                   => receive -> Received | Verdict | Waiting,
                Received                   => apply   -> Verdict,
                Verdict                    => report  -> Waiting,
            }
        }
    }

    /// Подтверждение образа, затем ожидание заголовка.
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
    /// обмен, о котором отправитель уже услышал `Ok`. Не удалось — флаг не
    /// ставится, и узел пробует ещё раз на следующем заголовке (`screen`).
    async fn begin(&mut self) -> Waiting {
        info!("domain: узел OTA запущен");
        self.confirm().await;
        Waiting
    }

    /// Подтверждение образа, если его ещё не было (см. `begin`).
    async fn confirm(&mut self) {
        if !*self.confirmed {
            match self.device.parts().1.mark_booted().await {
                Ok(()) => *self.confirmed = true,
                Err(err) => warn!("ota: не удалось подтвердить образ: {:?}", &err),
            }
        }
    }

    /// Проверки до стирания: занятость, затем режим (подпись).
    async fn screen(&mut self, _: Waiting, announce: Announce) -> edges::Screen {
        info!("ota: заголовок: {} байт", announce.len);
        // Не удалось подтвердить при старте — ещё раз перед новым образом, а
        // не чаще: неудавшийся `mark_booted` может стоить стирания `STATE`.
        self.confirm().await;
        // Занятость — до всего: адаптер и так откажет в `prepare`, но тогда
        // отправитель услышал бы `Device` вместо «сначала перезагрузи».
        let outcome = match self.device.parts().1.is_busy().await {
            Ok(false) => self.mode.check(&announce),
            Ok(true) => Err(Rejection::Busy),
            Err(err) => {
                warn!("ota: флеш не ответил о занятости: {:?}", &err);
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
        let (link, flash) = self.device.parts();
        match download::receive(link, flash, s.announce.len).await {
            Ok(len) => Received {
                announce: s.announce,
                len,
            }
            .into(),
            // Канал — без warn! о приёме: о нём скажет `lost`, сообщать
            // отправителю некому.
            Err(DownloadError::Source(err)) => self.lost(err).into(),
            Err(err) => {
                warn!("ota: приём не удался: {:?}", &err);
                Verdict {
                    outcome: Err(download_rejection(&err)),
                }
                .into()
            }
        }
    }

    /// Применяет принятый образ через режим (plain или signed).
    async fn apply(&mut self, s: &Received) -> Verdict {
        let outcome = self
            .mode
            .apply(self.device.parts().1, &s.announce, s.len)
            .await;
        if outcome.is_ok() {
            // Обмен запрошен (`STATE = Swap`): подтверждение текущего образа
            // теперь сняло бы запрос — и раз не вышло при старте, больше его не
            // пробуем (см. `begin`).
            *self.confirmed = true;
        }
        Verdict { outcome }
    }

    /// Исход — в лог и отправителю.
    async fn report(&mut self, s: Verdict) -> Waiting {
        match s.outcome {
            Ok(()) => info!("ota: образ принят, обмен разделов на следующем сбросе"),
            Err(rejection) => warn!("ota: образ отвергнут: {:?}", rejection),
        }
        match self.device.parts().0.finish(s.outcome).await {
            Ok(()) => Waiting,
            Err(err) => self.lost(err),
        }
    }

    /// Отказ канала: сообщать отправителю некому — в лог, и снова ждём
    /// заголовок; ждать, пока канал вернётся, — дело `ImageSource`.
    #[expect(clippy::unused_self, reason = "действие автомата")]
    fn lost(&mut self, err: <D::Link as ImageSource>::Error) -> Waiting {
        warn!("ota: отказ канала: {:?}", &err);
        Waiting
    }
}

/// Код отказа для отправителя по ошибке приёма. `Source` сюда не доходит —
/// `receive` перехватывает его раньше как отказ канала.
fn download_rejection<S, F>(err: &DownloadError<S, F>) -> Rejection {
    match err {
        DownloadError::Empty | DownloadError::TooLong { .. } => Rejection::Length,
        DownloadError::TooMuchData { .. } | DownloadError::Incomplete { .. } => Rejection::Transfer,
        // `UnusableGranularity`, `Flash` и незнакомые варианты (перечисление
        // `non_exhaustive`) сообщаются как отказ устройства, а не прячутся.
        _ => Rejection::Device,
    }
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
// `fragments: [::domain::OTA_FRAG<…> as OTA = ::domain::ota::OtaWiring { device: …, mode: … }]`,
// а `spawn_all` связывает её первой строкой пролога, и инициализаторы слотов
// ниже читают её поля. Так фрагмент не видит `Board` вовсе — только то, что
// ему дали. `MODE` — обычный слот, не `local`: перечисление из данных
// `Send` всегда. `CONFIRMED` — без входа: подтверждён ли уже образ в этом
// запуске (см. действие `begin`).
//
// `ctx:` — контекст [`OtaCtx`] эмитит фрагмент здесь, в `domain`; автомат
// стоит на нём (см. `domain::heartbeat`).
//
// `<D>` — параметр фрагмента: тип ресурсного слота — `static`, назвать
// его фрагмент обязан, а конкретный тип (`bsp::ota::Ota`: адаптер обновления и
// транспорт проекта) `domain` не видит и видеть не должен. Compose-site подставляет их в
// `fragments:` угловыми скобками, по позиции; bound'ов у параметров нет
// намеренно — их держит сигнатура `task`, и rustc проверяет её, а не копию в
// DSL. Всё остальное, что фрагмент называет, написано полным путём
// (`$crate::ota::Mode`, `::supervisor::…`) — от `graph.rs` он не требует
// ни одного объявления, см. `HEARTBEAT_FRAG`.
//
// `local` на слоте: разделы платы — `!Send` (`adapters::ota::Updater` над ними держит
// `&'static Mutex<NoopRawMutex, …>`, а `NoopRawMutex` намеренно не `Sync`),
// тогда как обычный слот — `static`, которому нужен `Send`. `local` меняет
// слот на `LocalResourceSlot` и снимает это требование, взамен обязывая
// держать значение на одном исполнителе — том, что спавнит граф. Это то же
// допущение, на котором держится `NoopRawMutex` у `FlashMutex`: один
// исполнитель, без вытеснения. Транспорт проекта внутри тоже вправе быть
// `!Send` (DMA-буферы, периферия), и править ради этого фрагмент здесь не
// придётся. Compose-site платит фичей `supervisor/local-resources` и
// `unsafe`-блоком на каждый `local`-`static` (здесь один), которые макрос
// эмитит в `app`. Узел, чей `local`-слот
// заполняется инициализатором (`= inputs.…`, как здесь), нельзя перевести на
// другой исполнитель (`executor:`): значение строится в прологе `spawn_all`
// на исполнителе графа, а забирать его с другого запрещено; `local` без
// инициализатора такого ограничения не несёт.
//
// Без `watchdog:` намеренно — см. `task`.
supervisor::supervisor_fragment! {
    name: OTA_FRAG<D>;
    boot: inputs: struct $crate::ota::OtaWiring;

    node deps: [], restart: Never,
        resources: [
            /// OTA платы: канал доставки образа (`ImageSource`) и адаптер
            /// обновления поверх разделов флеша (`FirmwareUpdate`) в одном объекте.
            DEVICE: local D = inputs.device,
            /// Режим применения: `Mode::Plain` или `Mode::Signed`.
            MODE: $crate::ota::Mode = inputs.mode,
            CONFIRMED: bool = false
        ],
        ctx: $crate::ota::OtaCtx,
        task: $crate::ota::task;
}

#[cfg(test)]
mod tests {
    use core::pin::pin;
    use core::task::{Context, Waker};

    use super::{Mode, OtaCtx, task};
    use crate::ota::fakes::{FakeFlash, FakeLink, SIGNATURE};
    use crate::ota::update::VERSION_BYTES;
    use crate::ota::version::pack;
    use ports::OtaDevice;
    use ports::{Announce, Rejection};

    /// Пара фейков под одним владельцем, как `adapters::ota::Ota` на плате.
    /// Заимствует их, чтобы тест после прогона видел, что сделал узел.
    struct Pair<'a> {
        link: &'a mut FakeLink,
        flash: &'a mut FakeFlash,
    }

    impl OtaDevice for Pair<'_> {
        type Link = FakeLink;
        type Flash = FakeFlash;

        fn parts(&mut self) -> (&mut FakeLink, &mut FakeFlash) {
            (&mut *self.link, &mut *self.flash)
        }
    }

    /// Гоняет узел, пока он не станет ждать следующий заголовок: заголовки
    /// кончились — `begin` у `FakeLink` ждёт вечно, как настоящий канал.
    /// `confirmed` — слот снаружи, как у второго прогона узла в том же запуске
    /// прошивки.
    fn serve_until_idle(
        link: &mut FakeLink,
        flash: &mut FakeFlash,
        mut mode: Mode,
        confirmed: &mut bool,
    ) {
        let mut device = Pair { link, flash };
        let ctx = OtaCtx {
            device: &mut device,
            mode: &mut mode,
            confirmed,
            index: 0,
        };
        let mut task = pin!(task(ctx));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(task.as_mut().poll(&mut cx).is_pending(), "ждёт заголовок");
    }

    fn serve(link: &mut FakeLink, flash: &mut FakeFlash, mode: Mode) {
        serve_until_idle(link, flash, mode, &mut false);
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
    #[test]
    fn applies_an_unsigned_image_and_reports_success() {
        let image = image(64);
        let mut link =
            FakeLink::of(image.chunks(13).map(<[u8]>::to_vec)).announcing([announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);

        serve(&mut link, &mut flash, plain());

        assert_eq!(flash.prepared, Some(64));
        assert_eq!(&flash.memory[..64], &image[..]);
        assert_eq!(flash.updated, 1, "обмен разделов запрошен один раз");
        assert_eq!(link.finished, vec![Ok(())]);
    }

    /// Образ подтверждается при старте узла — до первого заголовка, даже если
    /// канал отказал сразу: иначе bootloader откатил бы обновление на
    /// следующем сбросе.
    #[test]
    fn confirms_the_running_image_before_the_first_header() {
        let mut link = FakeLink::of([]);
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, plain());

        assert_eq!(flash.booted, 1);
        assert_eq!(flash.prepared, None, "раздел не тронут");
    }

    /// Рестарт узла после принятого образа (канал отказал уже после
    /// `mark_updated`) не подтверждает снова: иначе запрос на обмен, о котором
    /// отправитель услышал `Ok`, был бы молча снят.
    #[test]
    fn a_second_run_does_not_confirm_again() {
        let image = image(16);
        let mut confirmed = false;
        let mut flash = FakeFlash::new(64, 8);

        let mut link = FakeLink::of([image]).announcing([announce(16)]);
        serve_until_idle(&mut link, &mut flash, plain(), &mut confirmed);
        assert_eq!((flash.booted, flash.updated), (1, 1));

        let mut link = FakeLink::of([]);
        serve_until_idle(&mut link, &mut flash, plain(), &mut confirmed);
        assert_eq!(flash.booted, 1, "второй прогон не подтверждает");
    }

    /// Подтверждение не удалось, образов не было — узел работает дальше и
    /// пробует снова на следующем прогоне. (С принятым образом — нет: см.
    /// `an_accepted_image_is_never_followed_by_a_confirmation`.)
    #[test]
    fn a_failed_confirmation_is_retried_on_the_next_run() {
        let mut confirmed = false;
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_booted = Err("флеш не отвечает");

        let mut link = FakeLink::of([]);
        serve_until_idle(&mut link, &mut flash, plain(), &mut confirmed);
        assert!(!confirmed);

        flash.mark_booted = Ok(());
        let mut link = FakeLink::of([]);
        serve_until_idle(&mut link, &mut flash, plain(), &mut confirmed);
        assert!(confirmed);
    }

    /// Отказ подтверждения не мешает принять образ.
    #[test]
    fn a_failed_confirmation_does_not_block_an_update() {
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_booted = Err("флеш не отвечает");
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        serve(&mut link, &mut flash, plain());
        assert_eq!(link.finished, vec![Ok(())], "образ принят");
    }

    /// Занятость — до стирания и до чтения канала: адаптер и так отказал бы
    /// в `prepare`, но отправитель услышал бы `Device` вместо «перезагрузи».
    #[test]
    fn reports_busy_before_touching_the_partition_or_the_link() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);
        flash.busy = true;

        serve(&mut link, &mut flash, plain());

        assert_eq!(link.finished, vec![Err(Rejection::Busy)]);
        assert_eq!(flash.prepared, None, "раздел не должен быть стёрт");
        assert_eq!(link.next, 0, "куски не читались");
    }

    #[test]
    fn maps_a_bad_length_to_length_without_erasing() {
        let mut link = FakeLink::of([]).announcing([announce(65)]);
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, plain());

        assert_eq!(link.finished, vec![Err(Rejection::Length)]);
        assert_eq!(flash.prepared, None);
    }

    #[test]
    fn maps_excess_data_to_transfer() {
        let mut link = FakeLink::of([vec![0; 16], vec![0; 8]]).announcing([announce(16)]);
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, plain());

        assert_eq!(link.finished, vec![Err(Rejection::Transfer)]);
        assert_eq!(flash.updated, 0, "обмен не запрашивался");
    }

    /// Негодная гранулярность — вина устройства, не отправителя, и до
    /// стирания: `receive` отказывает, не трогая раздел.
    #[test]
    fn maps_an_unusable_granularity_to_device_without_erasing() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);
        flash.word = 0;

        serve(&mut link, &mut flash, plain());

        assert_eq!(link.finished, vec![Err(Rejection::Device)]);
        assert_eq!(flash.prepared, None);
    }

    #[test]
    fn maps_a_flash_failure_on_apply_to_device() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_updated = Err("флеш отказал");

        serve(&mut link, &mut flash, plain());

        assert_eq!(link.finished, vec![Err(Rejection::Device)]);
    }

    /// Обрыв канала посреди приёма: сообщать некому, `finish` не зовётся —
    /// ни на этом образе, ни на следующем, который рвётся там же.
    #[test]
    fn a_link_failure_while_receiving_goes_without_a_report() {
        let mut link =
            FakeLink::of([vec![0; 8], vec![0; 8]]).announcing([announce(16), announce(8)]);
        link.fail_at = Some(1);
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, plain());

        assert!(link.finished.is_empty(), "сообщать некому");
    }

    /// Обрыв при отчёте узел не останавливает: следующий заголовок
    /// читается — кусков на него не осталось, и он отвергнут.
    #[test]
    fn a_link_failure_on_finish_does_not_stop_the_node() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8), announce(8)]);
        link.finish = Err("обрыв при отчёте");
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, plain());

        assert_eq!(link.finished, vec![Ok(()), Err(Rejection::Transfer)]);
    }

    /// После отчёта автомат возвращается к заголовку: второй цикл идёт
    /// своим ходом — здесь кусков на него не осталось, и он отвергнут.
    #[test]
    fn serves_cycle_after_cycle() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8), announce(8)]);
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, plain());

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
    #[test]
    fn applies_a_signed_image_with_the_signature_from_the_announce() {
        let image = versioned_image(64, pack(1, 2, 4));
        let mut link = FakeLink::of([image.clone()]).announcing([signed_announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);

        serve(&mut link, &mut flash, signed());

        assert_eq!(&flash.memory[..64], &image[..]);
        assert_eq!(flash.recorded, vec![(SIGNATURE, 64)]);
        assert_eq!(flash.updated, 1);
        assert_eq!(link.finished, vec![Ok(())]);
    }

    /// Без подписи в заголовке — отказ ДО стирания: стирание уничтожило бы
    /// образ, в который устройство откатывается, а применить всё равно нечем.
    #[test]
    fn refuses_a_signed_update_without_a_signature_before_erasing() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, signed());

        assert_eq!(link.finished, vec![Err(Rejection::Signature)]);
        assert_eq!(flash.prepared, None, "раздел не должен быть стёрт");
        assert_eq!(link.next, 0, "куски не читались");
    }

    /// Та же версия — откат, и код для отправителя свой.
    #[test]
    fn maps_a_rollback_to_rollback() {
        let image = versioned_image(64, pack(1, 2, 3));
        let mut link = FakeLink::of([image]).announcing([signed_announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);

        serve(&mut link, &mut flash, signed());

        assert_eq!(link.finished, vec![Err(Rejection::Rollback)]);
        assert!(flash.recorded.is_empty(), "обмен не должен быть запрошен");
    }

    /// Заголовок короче хвоста с версией — `Length`, а не паника на вычитании.
    #[test]
    fn maps_a_signed_header_shorter_than_the_version_tail_to_length() {
        let mut link = FakeLink::of([vec![0; 3]]).announcing([signed_announce(3)]);
        let mut flash = FakeFlash::new(1024, 8);

        serve(&mut link, &mut flash, signed());

        assert_eq!(link.finished, vec![Err(Rejection::Length)]);
        assert!(flash.recorded.is_empty());
    }

    /// Длина больше вместимости — `Length` ещё в приёме, до стирания.
    #[test]
    fn maps_a_too_long_signed_image_to_length_without_erasing() {
        let mut link = FakeLink::of([]).announcing([signed_announce(65)]);
        let mut flash = FakeFlash::new(64, 8);

        serve(&mut link, &mut flash, signed());

        assert_eq!(link.finished, vec![Err(Rejection::Length)]);
        assert_eq!(flash.prepared, None);
    }

    /// Отказ флеша при записи для bootloader'а — `Device`, обмен не помечен.
    #[test]
    fn maps_a_flash_failure_on_the_signature_record_to_device() {
        let image = versioned_image(64, pack(1, 2, 4));
        let mut link = FakeLink::of([image]).announcing([signed_announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);
        flash.record = Err("флеш отказал");

        serve(&mut link, &mut flash, signed());

        assert_eq!(link.finished, vec![Err(Rejection::Device)]);
        assert_eq!(flash.updated, 0);
    }

    /// Не удалось подтвердить при старте — узел пробует ещё раз на каждом
    /// заголовке, а на отказе канала — нет: повтор стоит стирания `STATE`.
    #[test]
    fn a_failed_confirmation_is_retried_on_the_next_header() {
        let mut confirmed = false;
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_booted = Err("флеш не отвечает");
        // Образ длиннее раздела — отвергнут; отказ канала — на отчёте.
        let mut link = FakeLink::of([]).announcing([announce(1000)]);
        link.finish = Err("обрыв при отчёте");
        serve_until_idle(&mut link, &mut flash, plain(), &mut confirmed);
        assert_eq!(flash.confirm_attempts, 2, "при старте и на заголовке");
        assert!(!confirmed);
    }

    /// Принятый образ отменяет повтор подтверждения: после `mark_updated`
    /// `mark_booted` снял бы запрос на обмен, о котором отправитель уже
    /// услышал `Ok`.
    #[test]
    fn an_accepted_image_is_never_followed_by_a_confirmation() {
        let mut confirmed = false;
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_booted = Err("флеш не отвечает");
        let mut link = FakeLink::of([image(16)]).announcing([announce(16), announce(16)]);
        serve_until_idle(&mut link, &mut flash, plain(), &mut confirmed);
        assert_eq!(flash.updated, 1, "обмен запрошен");
        assert_eq!(
            flash.confirm_attempts, 2,
            "при старте и на первом заголовке, на втором — нет"
        );
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

    use std::collections::VecDeque;

    use ports::{Announce, FirmwareUpdate, ImageSource, PendingImage, Rejection};

    use crate::ota::update::VERSION_BYTES;

    /// Подпись, которую тесты кладут в заголовок; фейк её не проверяет, а
    /// записывает — важно, что до него доехала именно она.
    pub(super) const SIGNATURE: [u8; 64] = [9; 64];

    /// Канал, отдающий заранее нарезанные куски; на `fail_at`-м вызове `next`
    /// отказывает — так разыгрывается обрыв связи. Заголовки — по одному на
    /// цикл; когда они кончаются, `begin` ждёт вечно — на этом узел в тесте
    /// останавливается.
    pub(super) struct FakeLink {
        announces: VecDeque<Announce>,
        chunks: Vec<Vec<u8>>,
        /// Сколько кусков уже отдано — по нему тест видит, читался ли канал.
        pub(super) next: usize,
        pub(super) fail_at: Option<usize>,
        /// Что ответить на `finish`; `Err` разыгрывает обрыв при отчёте.
        pub(super) finish: Result<(), &'static str>,
        /// Всё, что узел сообщил отправителю, в порядке вызовов.
        pub(super) finished: Vec<Result<(), Rejection>>,
    }

    impl FakeLink {
        pub(super) fn of(chunks: impl IntoIterator<Item = Vec<u8>>) -> Self {
            Self {
                announces: VecDeque::new(),
                chunks: chunks.into_iter().collect(),
                next: 0,
                fail_at: None,
                finish: Ok(()),
                finished: Vec::new(),
            }
        }

        /// Заголовки, которые `begin` отдаст по одному на цикл.
        pub(super) fn announcing(mut self, announces: impl IntoIterator<Item = Announce>) -> Self {
            self.announces = announces.into_iter().collect();
            self
        }
    }

    impl ImageSource for FakeLink {
        type Error = &'static str;

        /// Заголовки кончились — ждёт следующего вечно, как настоящий канал.
        fn begin(&mut self) -> impl Future<Output = Result<Announce, Self::Error>> {
            let next = self.announces.pop_front();
            async move {
                match next {
                    Some(announce) => Ok(announce),
                    None => std::future::pending().await,
                }
            }
        }

        fn next(&mut self) -> impl Future<Output = Result<Option<&[u8]>, Self::Error>> {
            if self.fail_at == Some(self.next) {
                return std::future::ready(Err("обрыв канала"));
            }
            let i = self.next;
            self.next += 1;
            std::future::ready(Ok(self.chunks.get(i).map(Vec::as_slice)))
        }

        fn finish(
            &mut self,
            outcome: Result<(), Rejection>,
        ) -> impl Future<Output = Result<(), Self::Error>> {
            self.finished.push(outcome);
            std::future::ready(self.finish)
        }
    }

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
