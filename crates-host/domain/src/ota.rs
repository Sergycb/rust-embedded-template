//! Узел `OTA` графа задач: принимает образ по каналу и применяет его — в
//! цикле, пока жив канал.
//!
//! Здесь, а не в `crates-cross/app`, по общему правилу проекта (см.
//! `domain::app`): что узел делает — логика, и проверяется она на хосте на
//! фейках обоих портов. Проекту остаётся канал — реализация
//! [`ImageSource`] под свой транспорт (USB CDC, UART, сеть) в `bsp`:
//! заголовок, куски, отчёт отправителю — и одна строка в `fragments:` графа.
//!
//! Два входа — [`run`] и [`run_signed`] — потому что применение различается
//! по Cargo-варианту (`signed`), а Liquid в `domain` запрещён. Выбор делает
//! `crates-cross/app/src/graph.rs`: `OTA_FRAG` или `OTA_SIGNED_FRAG`. Обе
//! функции лежат в любом проекте; во flash попадает только позванная —
//! generic без вызова не мономорфизируется.
//!
//! Что узел делает с исходом, он не решает: [`ImageSource::finish`] получает
//! `Ok(())` или код отказа [`Rejection`], а ответить хосту, сбросить МК или и
//! то и другое — выбор реализации порта. Подробности — `docs/ota.md`.

use core::fmt::Debug;

use defmt_or_log::{Debug2Format, info, warn};
use ports::{Announce, DownloadError, FirmwareUpdate, ImageSource, Rejection, UpdateError};
use supervisor::runtime::TaskExit;

use crate::download;

/// Что узлу нужно на входе. Строит compose-site из полей `Board`:
///
/// ```ignore
/// fragments: [::domain::OTA_FRAG = ::domain::ota::Inputs { link: board.ota_link, flash: board.ota }];
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
    /// Адаптер обновления — `board.ota`.
    pub flash: F,
}

/// Узел `OTA` без проверки подписи: принятый образ применяется
/// [`FirmwareUpdate::mark_updated`].
///
/// Не возвращается, пока жив канал: после каждого цикла — удачного или нет —
/// ждёт следующий заголовок. `TaskExit::Failed` — только когда отказал сам
/// канал (`begin`, `next` или `finish`); граф перезапустит узел с backoff'ом,
/// и объект канала вернётся в слот к новому прогону. `Completed` не
/// возвращается никогда.
///
/// Без `watchdog:` намеренно: узел законно висит в `begin()` сколько угодно,
/// а опрашивать его с таймаутом нельзя — отмена future уронила бы
/// полупрочитанный заголовок. Железо кормит узел `APP`.
pub async fn run<S, F>(link: &mut S, flash: &mut F) -> TaskExit
where
    S: ImageSource,
    S::Error: Debug,
    F: FirmwareUpdate,
    F::Error: Debug,
{
    info!("domain: узел OTA запущен");
    serve(
        link,
        flash,
        &Mode {
            check: plain_check,
            apply: plain_apply,
        },
    )
    .await
}

/// Чем режимы отличаются: проверка заголовка до стирания и применение.
///
/// Указатели на функции, а не трейт: две пары по нескольку строк не стоят
/// публичной абстракции, а цикл при этом один — и тестируется один.
struct Mode<F> {
    /// Проверка заголовка ДО стирания раздела.
    check: fn(&Announce) -> Result<(), Rejection>,
    /// Применение принятого образа длиной `len`.
    apply: fn(&mut F, &Announce, u32) -> Result<(), Rejection>,
}

/// Циклы до отказа канала.
async fn serve<S, F>(link: &mut S, flash: &mut F, mode: &Mode<F>) -> TaskExit
where
    S: ImageSource,
    S::Error: Debug,
    F: FirmwareUpdate,
    F::Error: Debug,
{
    loop {
        if let Err(err) = cycle(link, flash, mode).await {
            warn!("ota: отказ канала: {:?}", Debug2Format(&err));
            return TaskExit::Failed;
        }
    }
}

/// Один цикл: заголовок → проверки до стирания → приём → применение → отчёт.
///
/// `Err` — отказал сам канал, и сообщать отправителю уже некому; всё
/// остальное ушло ему через `finish` кодом отказа.
async fn cycle<S, F>(link: &mut S, flash: &mut F, mode: &Mode<F>) -> Result<(), S::Error>
where
    S: ImageSource,
    S::Error: Debug,
    F: FirmwareUpdate,
    F::Error: Debug,
{
    let announce = link.begin().await?;
    info!("ota: заголовок: {} байт", announce.len);
    let outcome = attempt(link, flash, &announce, mode).await?;
    match outcome {
        Ok(()) => info!("ota: образ принят, обмен разделов на следующем сбросе"),
        Err(rejection) => warn!("ota: образ отвергнут: {:?}", rejection),
    }
    link.finish(outcome).await
}

/// От заголовка до применения. Внешний `Result` — канал
/// (`DownloadError::Source`), внутренний — вердикт для отправителя.
async fn attempt<S, F>(
    link: &mut S,
    flash: &mut F,
    announce: &Announce,
    mode: &Mode<F>,
) -> Result<Result<(), Rejection>, S::Error>
where
    S: ImageSource,
    S::Error: Debug,
    F: FirmwareUpdate,
    F::Error: Debug,
{
    // Занятость — до всего: адаптер и так откажет в `prepare`, но тогда
    // отправитель услышал бы `Device` вместо «сначала перезагрузи».
    match flash.is_busy() {
        Ok(false) => {}
        Ok(true) => return Ok(Err(Rejection::Busy)),
        Err(err) => {
            warn!("ota: флеш не ответил о занятости: {:?}", Debug2Format(&err));
            return Ok(Err(Rejection::Device));
        }
    }
    if let Err(rejection) = (mode.check)(announce) {
        return Ok(Err(rejection));
    }
    let len = match download::receive(link, flash, announce.len).await {
        Ok(len) => len,
        Err(err) => {
            warn!("ota: приём не удался: {:?}", Debug2Format(&err));
            return Ok(Err(download_rejection(err)?));
        }
    };
    Ok((mode.apply)(flash, announce, len))
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

/// Код отказа для отправителя; `Err` — отказал сам канал, и сообщать некому.
fn download_rejection<S, F>(err: DownloadError<S, F>) -> Result<Rejection, S> {
    Ok(match err {
        DownloadError::Empty | DownloadError::TooLong { .. } => Rejection::Length,
        DownloadError::TooMuchData { .. } | DownloadError::Incomplete { .. } => Rejection::Transfer,
        DownloadError::UnusableGranularity(_) | DownloadError::Flash(_) => Rejection::Device,
        DownloadError::Source(err) => return Err(err),
    })
}

/// Код отказа для отправителя по ошибке применения с подписью.
#[expect(dead_code, reason = "применение с подписью — задача 6")]
fn update_rejection<E>(err: &UpdateError<E>) -> Rejection {
    match err {
        UpdateError::TooLong { .. } | UpdateError::Truncated { .. } => Rejection::Length,
        UpdateError::Busy => Rejection::Busy,
        UpdateError::Rollback { .. } => Rejection::Rollback,
        UpdateError::NoPublicKey | UpdateError::BadSignature => Rejection::Signature,
        UpdateError::Flash(_) => Rejection::Device,
    }
}

#[cfg(test)]
mod tests {
    use super::{Mode, cycle, plain_apply, plain_check, run};
    use crate::test_support::{FakeFlash, FakeLink, block_on};
    use ports::{Announce, Rejection};
    use supervisor::runtime::TaskExit;

    fn plain() -> Mode<FakeFlash> {
        Mode {
            check: plain_check,
            apply: plain_apply,
        }
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
    #[test]
    fn applies_an_unsigned_image_and_reports_success() {
        let image = image(64);
        let mut link =
            FakeLink::of(image.chunks(13).map(<[u8]>::to_vec)).announcing([announce(64)]);
        let mut flash = FakeFlash::new(1024, 8);

        block_on(cycle(&mut link, &mut flash, &plain())).expect("канал жив");

        assert_eq!(flash.prepared, Some(64));
        assert_eq!(&flash.memory[..64], &image[..]);
        assert_eq!(flash.updated, 1, "обмен разделов запрошен один раз");
        assert_eq!(link.finished, vec![Ok(())]);
    }

    /// Занятость — до стирания и до чтения канала: адаптер и так отказал бы
    /// в `prepare`, но отправитель услышал бы `Device` вместо «перезагрузи».
    #[test]
    fn reports_busy_before_touching_the_partition_or_the_link() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);
        flash.busy = true;

        block_on(cycle(&mut link, &mut flash, &plain())).expect("канал жив");

        assert_eq!(link.finished, vec![Err(Rejection::Busy)]);
        assert_eq!(flash.prepared, None, "раздел не должен быть стёрт");
        assert_eq!(link.next, 0, "куски не читались");
    }

    #[test]
    fn maps_a_bad_length_to_length_without_erasing() {
        let mut link = FakeLink::of([]).announcing([announce(65)]);
        let mut flash = FakeFlash::new(64, 8);

        block_on(cycle(&mut link, &mut flash, &plain())).expect("канал жив");

        assert_eq!(link.finished, vec![Err(Rejection::Length)]);
        assert_eq!(flash.prepared, None);
    }

    #[test]
    fn maps_excess_data_to_transfer() {
        let mut link = FakeLink::of([vec![0; 16], vec![0; 8]]).announcing([announce(16)]);
        let mut flash = FakeFlash::new(64, 8);

        block_on(cycle(&mut link, &mut flash, &plain())).expect("канал жив");

        assert_eq!(link.finished, vec![Err(Rejection::Transfer)]);
        assert_eq!(flash.updated, 0, "обмен не запрашивался");
    }

    #[test]
    fn maps_a_flash_failure_on_apply_to_device() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);
        flash.mark_updated = Err("флеш отказал");

        block_on(cycle(&mut link, &mut flash, &plain())).expect("канал жив");

        assert_eq!(link.finished, vec![Err(Rejection::Device)]);
    }

    /// Обрыв канала посреди приёма: сообщать некому, цикл кончается ошибкой
    /// канала, `finish` не зовётся.
    #[test]
    fn a_link_failure_while_receiving_ends_the_cycle_without_a_report() {
        let mut link = FakeLink::of([vec![0; 8], vec![0; 8]]).announcing([announce(16)]);
        link.fail_at = Some(1);
        let mut flash = FakeFlash::new(64, 8);

        let failed = block_on(cycle(&mut link, &mut flash, &plain()));

        assert_eq!(failed, Err("обрыв канала"));
        assert!(link.finished.is_empty(), "сообщать некому");
    }

    #[test]
    fn a_link_failure_on_finish_ends_the_cycle() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        link.finish = Err("обрыв при отчёте");
        let mut flash = FakeFlash::new(64, 8);

        let failed = block_on(cycle(&mut link, &mut flash, &plain()));

        assert_eq!(failed, Err("обрыв при отчёте"));
    }

    /// `run` крутит циклы, пока жив канал, и выходит `Failed` на его отказе —
    /// здесь на исчерпании сценария: второй `begin` отвечает отказом.
    #[test]
    fn run_serves_until_the_link_fails() {
        let mut link = FakeLink::of([vec![0; 8]]).announcing([announce(8)]);
        let mut flash = FakeFlash::new(64, 8);

        let exit = block_on(run(&mut link, &mut flash));

        assert_eq!(exit, TaskExit::Failed);
        assert_eq!(link.finished, vec![Ok(())], "первый цикл дошёл до отчёта");
    }
}
