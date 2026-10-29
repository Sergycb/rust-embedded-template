//! Host-тесты узла `OTA` на фейках портов: сценарии команд через входы
//! `CMD`/`ASK` и стенды узла для тестов протоколов (`live`, `serving`).

use core::pin::{Pin, pin};
use core::task::{Context, Poll, Waker};

use embassy_time::{Duration, MockDriver};
use ports::{Chunk, OtaCommand, OtaReply, Progress, Reboot, Rejection};
use supervisor::runtime::TaskExit;

// {%- if has_modbus == "true" or has_protobuf == "true" or net_services contains "mqtt" %}
use super::OtaClient;
// {%- endif %}

use super::{ClientId, Mode, OtaRequest, REBOOT_GRACE, SESSION_TIMEOUT, task};
/// Отпечаток раскладки фейка — образы тестов узла и протоколов несут его
/// в хвосте.
pub(crate) use crate::ota::fakes::LAYOUT;
use crate::ota::fakes::{FakeFlash, SIGNATURE};
use crate::ota::update::{LAYOUT_BYTES, VERSION_BYTES};
use crate::ota::version::pack;

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
    Send(OtaRequest),
    /// Та же команда, но через вход `ASK`.
    Ask(OtaRequest),
    Wait(Duration),
    Skip(Duration),
}

// {%- if has_modbus == "true" or has_protobuf == "true" or net_services contains "mqtt" %}
/// Узел OTA, который тест крутит сам: `poll` по очереди с запросом.
pub(crate) type Node = Pin<Box<dyn Future<Output = TaskExit>>>;

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
pub(crate) fn live() -> (OtaClient, Node) {
    let stand = Box::leak(Box::new(
        supervisor::stand!(OTA_FRAG<FakeFlash, FakeReboot>; QUEUE_ASK = 1),
    ));
    let node = task(stand.ctx(
        Box::leak(Box::new(FakeFlash::new(1024, 8))),
        Box::leak(Box::new(FakeReboot)),
        Box::leak(Box::new(Mode::Plain)),
        Box::leak(Box::new(false)),
    ));
    (
        OtaClient::new(stand.cmd.requester(), ClientId(0)),
        Box::pin(node),
    )
}
// {%- endif %}

// {%- if has_canopen == "true" %}
/// Узел OTA на фейке флеша, чей вход `ASK` — `ask`: сервер ребра из стенда
/// узла-клиента (`stand.ota_ask` у фрагмента с `asks: [OTA.ASK ..]`).
pub(crate) fn serving<'a>(
    ask: supervisor::AskServer<'a, OtaRequest, OtaReply>,
) -> Pin<Box<dyn Future<Output = TaskExit> + 'a>> {
    let stand = Box::leak(Box::new(
        supervisor::stand!(OTA_FRAG<FakeFlash, FakeReboot>; QUEUE_ASK = 1),
    ));
    Box::pin(task(super::OtaCtx {
        flash: Box::leak(Box::new(FakeFlash::new(1024, 8))),
        reboot: Box::leak(Box::new(FakeReboot)),
        mode: Box::leak(Box::new(Mode::Plain)),
        confirmed: Box::leak(Box::new(false)),
        cmd: stand.cmd.server().expect("сервер CMD стенда свободен"),
        ask,
        ready: supervisor::Readiness::new(|| {}),
        index: 0,
    }))
}
// {%- endif %}

/// Команда от клиента 0 — единственного транспорта.
fn send(command: OtaCommand) -> Step {
    Step::Send(OtaRequest {
        client: ClientId(0),
        command,
    })
}

/// Та же команда от клиента `client`.
fn by(client: u8, step: Step) -> Step {
    let Step::Send(request) = step else {
        panic!("by() — только для команды");
    };
    Step::Send(OtaRequest {
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
    mut node: Pin<&mut impl Future<Output = TaskExit>>,
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
/// `confirmed` — слот снаружи, как у второго прогона узла в том же
/// запуске прошивки.
fn run_on(
    flash: &mut FakeFlash,
    mut mode: Mode,
    confirmed: &mut bool,
    steps: Vec<Step>,
) -> Vec<OtaReply> {
    MockDriver::get().reset();
    let stand = supervisor::stand!(OTA_FRAG<FakeFlash, FakeReboot>; QUEUE_ASK = 1);
    let mut reboot = FakeReboot;
    let requester = stand.cmd.requester();
    let mut asker = stand.ask();
    let mut node = pin!(task(stand.ctx(flash, &mut reboot, &mut mode, confirmed)));
    assert!(poll(node.as_mut()).is_pending(), "узел ждёт команду");

    let mut replies = Vec::new();
    for step in steps {
        let reply = match step {
            Step::Send(request) => {
                await_reply(node.as_mut(), pin!(requester.request(request))).expect("узел ответил")
            }
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

fn run(flash: &mut FakeFlash, mode: Mode, steps: Vec<Step>) -> Vec<OtaReply> {
    run_on(flash, mode, &mut false, steps)
}

#[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
fn start(len: usize) -> Step {
    send(OtaCommand::Start {
        len: len as u32,
        signature: None,
    })
}

#[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
fn signed_start(len: usize) -> Step {
    send(OtaCommand::Start {
        len: len as u32,
        signature: Some(SIGNATURE),
    })
}

fn chunk(offset: u32, data: &[u8]) -> Step {
    send(OtaCommand::Chunk(
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
    send(OtaCommand::Finish)
}

/// Образ с отпечатком раскладки фейка ([`LAYOUT`]) перед местом версии —
/// без него `Finish` отвергался бы в обоих режимах.
#[expect(clippy::cast_possible_truncation, reason = "размеры в тестах малы")]
fn image(len: usize) -> Vec<u8> {
    let mut image: Vec<u8> = (0..len).map(|i| (i * 3) as u8).collect();
    let layout = len - (LAYOUT_BYTES + VERSION_BYTES) as usize;
    image[layout..layout + LAYOUT_BYTES as usize].copy_from_slice(&LAYOUT.to_le_bytes());
    image
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

/// Готовность — только когда подтверждение закончилось: узлу, который её
/// ждёт, нельзя стартовать, пока подтверждение стирает `STATE`.
#[test]
fn is_ready_only_after_the_confirmation() {
    MockDriver::get().reset();
    let mut flash = FakeFlash::new(64, 8);
    flash.confirm_takes = Some(Duration::from_millis(1000));
    // Свой `stand!` на тест: готовность — `static` на месте вызова.
    let stand = supervisor::stand!(OTA_FRAG<FakeFlash, FakeReboot>; QUEUE_ASK = 1);
    let (mut reboot, mut mode, mut confirmed) = (FakeReboot, Mode::Plain, false);
    let mut node = pin!(task(stand.ctx(
        &mut flash,
        &mut reboot,
        &mut mode,
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
    let mut flash = FakeFlash::new(64, 8);
    flash.mark_booted = Err("флеш не отвечает");
    let stand = supervisor::stand!(OTA_FRAG<FakeFlash, FakeReboot>; QUEUE_ASK = 1);
    let (mut reboot, mut mode, mut confirmed) = (FakeReboot, Mode::Plain, false);
    let mut node = pin!(task(stand.ctx(
        &mut flash,
        &mut reboot,
        &mut mode,
        &mut confirmed
    )));

    assert!(poll(node.as_mut()).is_pending());
    assert!(stand.is_ready());
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
        vec![start(8), chunk(0, &image(8)), finish()],
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
        vec![start(16), chunk(0, &image(8)), finish()],
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
        vec![start(8), chunk(0, &image(8)), finish()],
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

/// Чужой `Start` посреди приёма — `Busy`, приём владельца не тронут.
#[test]
fn a_foreign_start_during_receiving_is_busy() {
    let mut flash = FakeFlash::new(64, 8);
    let image = image(16);

    let replies = run(
        &mut flash,
        Mode::Plain,
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
    assert_eq!(flash.prepared, Some(16), "чужой Start раздел не готовил");
    assert_eq!(&flash.memory[..16], &image[..]);
}

/// Чужой кусок с тем смещением, что ждёт владелец, — `Busy` и не
/// записан; кусок владельца следом принят.
#[test]
fn a_foreign_chunk_is_busy_and_not_written() {
    let mut flash = FakeFlash::new(64, 8);
    let image = image(16);

    let replies = run(
        &mut flash,
        Mode::Plain,
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
    assert_eq!(&flash.memory[..16], &image[..]);
}

/// Входы `CMD` и `ASK` — один автомат: приём, начатый через `ASK`,
/// идёт через него, а команда клиента с `CMD` получает `Busy`.
#[test]
fn both_inputs_drive_one_session() {
    let mut flash = FakeFlash::new(64, 8);
    let image = image(16);

    let replies = run(
        &mut flash,
        Mode::Plain,
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
    assert_eq!(&flash.memory[..16], &image[..]);
}

/// Чужой `Finish` — `Busy`, приём продолжается.
#[test]
fn a_foreign_finish_is_busy() {
    let mut flash = FakeFlash::new(64, 8);

    let replies = run(
        &mut flash,
        Mode::Plain,
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
    assert_eq!(flash.updated, 1, "обмен запрошен одним Finish владельца");
}

/// Владелец повторил `Start` (ответ потерялся) — приём с нуля, не `Busy`.
#[test]
fn the_owner_may_restart_its_session() {
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
    assert_eq!(flash.prepared, Some(16));
}

/// Владелец замолчал — по сроку от его последней команды сессия свободна;
/// чужой `Start` посреди срока его не продлевает.
#[test]
fn a_silent_owner_releases_the_session() {
    let mut flash = FakeFlash::new(64, 8);
    let half = SESSION_TIMEOUT / 2;

    let replies = run(
        &mut flash,
        Mode::Plain,
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
    assert_eq!(flash.prepared, Some(8), "сессия клиента 1");
}

/// Чужой `Start` после срока владельца, но раньше, чем узел заметил срок
/// (команда выиграла опрос у `expired`), — не `Busy`: приём владельца
/// брошен, начинается приём нового клиента.
#[test]
fn a_foreign_start_past_the_owners_deadline_takes_over() {
    let mut flash = FakeFlash::new(64, 8);

    let replies = run(
        &mut flash,
        Mode::Plain,
        vec![start(16), Step::Skip(SESSION_TIMEOUT), by(1, start(8))],
    );

    assert_eq!(replies, vec![Ok(Progress::Started), Ok(Progress::Started)]);
    assert_eq!(flash.prepared, Some(8), "сессия клиента 1");
}

/// `Reboot` обслуживается от любого клиента, и посреди чужого приёма тоже.
#[test]
fn reboot_is_served_for_any_client() {
    let mut flash = FakeFlash::new(64, 8);

    let replies = run(
        &mut flash,
        Mode::Plain,
        vec![start(16), by(1, send(OtaCommand::Reboot))],
    );

    assert_eq!(
        replies,
        vec![Ok(Progress::Started), Ok(Progress::Rebooting)]
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
            send(OtaCommand::Reboot),
            Step::Wait(REBOOT_GRACE - Duration::from_millis(1)),
        ],
    );
    assert_eq!(replies, vec![Ok(Progress::Rebooting)], "ответ — до сброса");

    run(
        &mut flash,
        Mode::Plain,
        vec![send(OtaCommand::Reboot), Step::Wait(REBOOT_GRACE)],
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

/// Образ короче хвоста (раскладка и версия) — `Length`, а не паника на
/// вычитании.
#[test]
fn maps_a_signed_image_shorter_than_the_tail_to_length() {
    let mut flash = FakeFlash::new(1024, 8);

    let replies = run(&mut flash, signed(), signed_update(&[0; 7]));

    assert_eq!(replies.last(), Some(&Err(Rejection::Length)));
    assert!(flash.recorded.is_empty());
}

/// Чужая раскладка с подписью — `Layout`, а не `Rollback`, хотя версия
/// новее; обмен не запрошен.
#[test]
fn maps_a_signed_image_for_another_layout_to_layout() {
    let mut image = versioned_image(64, pack(1, 2, 4));
    image[56..60].copy_from_slice(&(!LAYOUT).to_le_bytes());
    let mut flash = FakeFlash::new(1024, 8);

    let replies = run(&mut flash, signed(), signed_update(&image));

    assert_eq!(replies.last(), Some(&Err(Rejection::Layout)));
    assert!(flash.recorded.is_empty());
    assert_eq!(flash.updated, 0);
}

/// Без подписи раскладка сверяется тоже: чужая — `Layout`, обмен не
/// запрошен, приём окончен.
#[test]
fn refuses_an_unsigned_image_for_another_layout_before_marking_the_swap() {
    let mut image = image(64);
    image[56..60].copy_from_slice(&(!LAYOUT).to_le_bytes());
    let mut flash = FakeFlash::new(1024, 8);
    let mut steps = vec![start(64)];
    steps.extend(chunks(&image));
    steps.extend([finish(), chunk(0, &[0; 8])]);

    let replies = run(&mut flash, Mode::Plain, steps);

    assert_eq!(
        replies[replies.len() - 2..],
        [Err(Rejection::Layout), Err(Rejection::NoSession)]
    );
    assert_eq!(flash.updated, 0, "обмен под чужую раскладку не помечается");
}

/// Без подписи образ короче хвоста — `Length`, обмен не запрошен.
#[test]
fn refuses_an_unsigned_image_shorter_than_the_tail() {
    let mut flash = FakeFlash::new(1024, 8);
    let steps = vec![start(7), chunk(0, &[0; 7]), finish()];

    let replies = run(&mut flash, Mode::Plain, steps);

    assert_eq!(replies.last(), Some(&Err(Rejection::Length)));
    assert_eq!(flash.updated, 0);
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
    /// `Ready`. Сценарии выше проверяют по одной истории, здесь их тысячи.
    #[test]
    fn any_command_sequence_gets_one_reply_each_and_swaps_only_on_ready(
        steps in proptest::collection::vec(arbitrary_step(), 1..24)
    ) {
        let commands = steps.iter().filter(|step| !matches!(step, Step::Wait(_))).count();
        let mut flash = FakeFlash::new(1024, 8);
        let replies = run(&mut flash, Mode::Plain, steps);
        proptest::prop_assert_eq!(replies.len(), commands);
        let ready = replies.iter().filter(|reply| **reply == Ok(Progress::Ready)).count();
        proptest::prop_assert_eq!(flash.updated, ready);
    }
}
