//! Обработчик сообщений хоста по умолчанию — над примером схемы
//! `crates-host/proto`.

use embassy_time::Instant;
use ports::ProtobufService;
// {%- if ota == "true" %}
use core::array::TryFromSliceError;
use core::fmt::Write as _;
use ports::{Chunk, OtaCommand, OtaReply, Route, SIGNATURE_BYTES};
// {%- endif %}
use proto::{Error, Info, Request, Request_, Response, Response_, heapless};

pub use crate::device::DeviceInfo;

// Поле `uid` схемы — ровно заводской номер: копируется без проверки длины.
const _: () = assert!(proto::UID_BYTES == crate::device::UID_BYTES);
// Прошлую панику `app` сжимает до `PANIC_BRIEF` — она помещается в поле.
const _: () = assert!(crate::device::PANIC_BRIEF <= proto::PANIC_BYTES);

/// Самый длинный ответ схемы, байт: под него `app` заводит буфер ответа узла
/// (`PROTOBUF_MESSAGE` в `crates-cross/app/src/link.rs`, `const`-assert там
/// же). Не поместился бы — узел предупредил бы на старте, а такие ответы не
/// ушли бы.
pub const RESPONSE_MAX: usize = match <Response as micropb::MessageEncode>::MAX_SIZE {
    Ok(max) => max,
    Err(_) => panic!("у ответа схемы нет предела длины"),
};

/// Обработчик, с которым стартует узел `PROTOBUF` шаблона: на `info` —
/// сведения об устройстве{% if ota == "true" %}, команды обновления — узлу OTA ([`route`](ProtobufService::route)),
/// их исход — `ok` или `error`{% endif %}, `request_id` — обратно.
///
/// Это отправная точка, а не готовая логика: проект меняет схему в
/// `crates-host/proto` и команды здесь — перевод сообщений схемы в свои
/// проверенные типы и ответы на них.
///
/// Команда, которой нужно что-то от платы, получает это параметром типа —
/// портом (`ports::…`), а не железом: тогда её ответ проверяется на хосте
/// заглушкой порта. Узел `PROTOBUF` (`PROTOBUF_FRAG`) уже generic по
/// обработчику, менять его не нужно; конкретный тип подставляет приложение. Набросок (`ports::Relay` —
/// порт проекта, в шаблоне его нет):
///
/// ```ignore
/// pub struct Service<R> {
///     info: DeviceInfo,
///     relay: R,
/// }
///
/// impl<R: ports::Relay> ProtobufService for Service<R> {
///     // `Request_::Relay(on)` → `self.relay.set(on).await` → `Ok` или `Error`.
/// }
///
/// // Граф: `PROTOBUF_FRAG<ProtobufLink, ::domain::protobuf::Service<bsp::relay::Relay>>`.
/// // Тест: `Service::new(INFO, FakeRelay::default())` и ответ на запрос.
/// ```
#[derive(Debug)]
pub struct Service {
    info: DeviceInfo,
}

impl Service {
    /// Обработчик, отвечающий на `info` сведениями `info`.
    ///
    /// # Panics
    ///
    /// Если строка `info` длиннее ёмкости своего поля в схеме (`proto::*_BYTES`):
    /// обрезанная выглядела бы как настоящая, а пустую ответ не заслуживает. В
    /// `const`-контексте — ошибка компиляции.
    #[must_use]
    pub const fn new(info: DeviceInfo) -> Self {
        assert!(
            info.version.len() <= proto::VERSION_BYTES,
            "версия не помещается в Info.version"
        );
        assert!(
            info.commit.len() <= proto::COMMIT_BYTES,
            "коммит не помещается в Info.commit"
        );
        assert!(
            info.chip.len() <= proto::CHIP_BYTES,
            "имя чипа не помещается в Info.chip"
        );
        if let Some(panic) = info.last_panic {
            assert!(
                panic.len() <= proto::PANIC_BYTES,
                "паника не помещается в Info.last_panic"
            );
        }
        Self { info }
    }

    fn info(&self) -> Info {
        let info = &self.info;
        Info {
            version: text(info.version),
            commit: text(info.commit),
            chip: text(info.chip),
            // Ёмкость поля — `UID_BYTES`, длина массива — та же: не откажет.
            uid: heapless::Vec::from_slice(&info.uid).unwrap_or_default(),
            flash_size: info.flash_size,
            sys_hz: info.sys_hz.unwrap_or(0),
            // Часы `embassy-time` идут с запуска: их отсчёт и есть время работы.
            // Секунды в `u32` — 136 лет; 64-битные поля `micropb` кодирует
            // только с фичей `enable-64bit`.
            uptime_s: u32::try_from(Instant::now().as_secs()).unwrap_or(u32::MAX),
            reset: text(info.reset.as_str()),
            last_panic: text(info.last_panic.unwrap_or("")),
        }
    }
}

impl ProtobufService for Service {
    type Request = Request;
    type Response = Response;

    // `fn` с готовым future, а не `async fn`: шаблонным командам ждать нечего.
    // Команда, которая ждёт задачу, делает его `async fn` с `.await`.
    fn handle(&mut self, request: Request) -> impl Future<Output = Response> {
        let result = match request.command {
            Some(Request_::Command::Info(_)) => Response_::Result::Info(self.info()),
            // {%- if ota == "true" %}
            // Команды обновления сюда не доходят: их забирает `route`.
            Some(
                Request_::Command::OtaStart(_)
                | Request_::Command::OtaChunk(_)
                | Request_::Command::OtaFinish(_)
                | Request_::Command::Reboot(_),
            ) => error("update command not routed"),
            // {%- endif %}
            None => error("unknown command"),
        };
        core::future::ready(response(request.request_id, result))
    }

    fn malformed(&mut self) -> Response {
        response(0, error("bad frame"))
    }
    // {%- if ota == "true" %}

    fn route(&mut self, request: Request) -> Route<Request, Response> {
        let request_id = request.request_id;
        let command = match &request.command {
            Some(Request_::Command::OtaStart(start)) => match signature(&start.signature) {
                Ok(signature) => OtaCommand::Start {
                    len: start.len,
                    signature,
                },
                Err(_) => {
                    return Route::Reply(response(request_id, error("signature must be 64 bytes")));
                }
            },
            Some(Request_::Command::OtaChunk(chunk)) => {
                match Chunk::new(chunk.offset, &chunk.data) {
                    Some(chunk) => OtaCommand::Chunk(chunk),
                    // Ёмкость поля в схеме та же, что у куска: сюда не доходит.
                    None => return Route::Reply(response(request_id, error("chunk too long"))),
                }
            }
            Some(Request_::Command::OtaFinish(_)) => OtaCommand::Finish,
            Some(Request_::Command::Reboot(_)) => OtaCommand::Reboot,
            _ => return Route::Handle(request),
        };
        Route::Ota {
            request_id,
            command,
        }
    }

    fn ota_outcome(&mut self, request_id: u32, reply: OtaReply) -> Response {
        response(request_id, ota_reply(reply))
    }
    // {%- endif %}
}
// {%- if ota == "true" %}

// Пределы полей схемы (`proto/build.rs`) — те же, что у команды узлу OTA:
// разойдутся — кусок или подпись, прошедшие разбор, не дойдут до узла.
const _: () = assert!(
    proto::OTA_CHUNK_BYTES == ports::CHUNK_BYTES && proto::OTA_SIGNATURE_BYTES == SIGNATURE_BYTES,
    "пределы OtaChunk.data/OtaStart.signature в proto/build.rs разошлись с ports"
);

/// Подпись из `ota_start`: пусто — без подписи, 64 байта — подпись, иначе
/// отказ: обрезанную подпись принять за целую нельзя.
fn signature(bytes: &[u8]) -> Result<Option<[u8; SIGNATURE_BYTES]>, TryFromSliceError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    bytes.try_into().map(Some)
}

/// Исход команды обновления — `ok` или `error` с именем отказа
/// (`ports::Rejection`).
fn ota_reply(reply: OtaReply) -> Response_::Result {
    match reply {
        Ok(_) => Response_::Result::Ok(proto::Ok {}),
        Err(rejection) => {
            let mut text = heapless::String::new();
            // Имя варианта короче `ERROR_TEXT_BYTES`; не влезло бы — пустой
            // текст, как у `text`.
            if write!(text, "{rejection:?}").is_err() {
                text.clear();
            }
            Response_::Result::Error(Error { text })
        }
    }
}
// {%- endif %}

/// Ответ на запрос `request_id`.
fn response(request_id: u32, result: Response_::Result) -> Response {
    Response {
        request_id,
        result: Some(result),
    }
}

/// Ответ-отказ с текстом для человека.
fn error(message: &str) -> Response_::Result {
    Response_::Result::Error(Error {
        text: text(message),
    })
}

/// Строка схемы из `s`; не помещается в предел поля (`build.rs` крейта
/// `proto`) — пустая: обрезанная строка выглядела бы как настоящая. Строки
/// [`DeviceInfo`] длиннее предела отсекает [`Service::new`], так что это
/// касается только текстов ошибок.
fn text<const N: usize>(s: &str) -> heapless::String<N> {
    heapless::String::try_from(s).unwrap_or_default()
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    /// Сведения, которыми тесты собирают обработчик.
    pub(crate) const INFO: DeviceInfo = DeviceInfo {
        version: "1.2.3",
        commit: "abcdef12",
        chip: "stm32f407ve",
        uid: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        flash_size: 512 * 1024,
        sys_hz: None,
        reset: crate::device::ResetCause::Pin,
        last_panic: None,
    };

    /// Обработчик со сведениями `info`.
    pub(crate) fn service(info: DeviceInfo) -> Service {
        Service::new(info)
    }

    #[test]
    fn info_carries_every_field() {
        let info = service(INFO).info();
        assert_eq!(info.version, "1.2.3");
        assert_eq!(info.commit, "abcdef12");
        assert_eq!(info.chip, "stm32f407ve");
        assert_eq!(info.uid.as_slice(), &INFO.uid);
        assert_eq!(info.flash_size, 512 * 1024);
        assert_eq!(info.sys_hz, 0, "неизвестная частота — 0");
        assert_eq!(info.reset, "pin");
        assert_eq!(info.last_panic, "", "паники не было — пусто");
        let info = service(DeviceInfo {
            last_panic: Some("main.rs:7:1:\nboom"),
            ..INFO
        })
        .info();
        assert_eq!(info.last_panic, "main.rs:7:1:\nboom");
    }

    /// Имя любой причины сброса помещается в `Info.reset`: не поместилось бы —
    /// ушла бы пустая строка.
    #[test]
    fn every_reset_cause_fits_its_field() {
        use crate::device::ResetCause;
        // Место варианта в списке — `match` без `_`: новый вариант не
        // скомпилируется, пока ему не дадут место, а список сверяется с ним.
        let place = |cause: ResetCause| match cause {
            ResetCause::LowPower => 0,
            ResetCause::WindowWatchdog => 1,
            ResetCause::IndependentWatchdog => 2,
            ResetCause::Software => 3,
            ResetCause::OptionBytes => 4,
            ResetCause::PowerOn => 5,
            ResetCause::Pin => 6,
            ResetCause::Unknown => 7,
        };
        let all = [
            ResetCause::LowPower,
            ResetCause::WindowWatchdog,
            ResetCause::IndependentWatchdog,
            ResetCause::Software,
            ResetCause::OptionBytes,
            ResetCause::PowerOn,
            ResetCause::Pin,
            ResetCause::Unknown,
        ];
        for (i, cause) in all.into_iter().enumerate() {
            assert_eq!(place(cause), i, "{cause:?} не на своём месте в списке");
            assert!(cause.as_str().len() <= proto::RESET_BYTES, "{cause:?}");
        }
    }

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_panic_over_the_limit_is_refused() {
        let panic: &'static str = "p".repeat(proto::PANIC_BYTES + 1).leak();
        let _service = service(DeviceInfo {
            last_panic: Some(panic),
            ..INFO
        });
    }

    #[test]
    fn info_reports_uptime() {
        let before = service(INFO).info().uptime_s;
        embassy_time::MockDriver::get().advance(embassy_time::Duration::from_secs(2));
        assert!(service(INFO).info().uptime_s >= before + 2);
    }

    #[test]
    fn a_version_of_exactly_the_limit_is_accepted() {
        let version: &'static str = "v".repeat(proto::VERSION_BYTES).leak();
        let _service = service(DeviceInfo { version, ..INFO });
    }

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_version_over_the_limit_is_refused() {
        let version: &'static str = "v".repeat(proto::VERSION_BYTES + 1).leak();
        let _service = service(DeviceInfo { version, ..INFO });
    }

    // {%- if ota == "true" %}
    fn request(command: Request_::Command) -> Request {
        Request {
            request_id: 7,
            command: Some(command),
        }
    }

    /// Команды обновления уходят узлу OTA с номером запроса, остальное —
    /// на месте.
    #[test]
    fn update_commands_are_routed_to_the_ota_node() {
        let mut service = service(INFO);
        let start = service.route(request(Request_::Command::OtaStart(proto::OtaStart {
            len: 8,
            signature: heapless::Vec::new(),
        })));
        assert!(matches!(
            start,
            Route::Ota {
                request_id: 7,
                command: OtaCommand::Start {
                    len: 8,
                    signature: None
                }
            }
        ));
        let finish = service.route(request(Request_::Command::OtaFinish(proto::OtaFinish {})));
        assert!(matches!(
            finish,
            Route::Ota {
                command: OtaCommand::Finish,
                ..
            }
        ));
        let info = service.route(request(Request_::Command::Info(proto::Get::default())));
        assert!(matches!(info, Route::Handle(_)));
    }

    /// Подпись не той длины — ответ-отказ сразу, до узла OTA она не доходит.
    #[test]
    fn a_signature_of_the_wrong_length_is_refused() {
        let route = service(INFO).route(request(Request_::Command::OtaStart(proto::OtaStart {
            len: 8,
            signature: heapless::Vec::from_slice(&[0; 8]).unwrap(),
        })));
        let Route::Reply(reply) = route else {
            panic!("ожидался ответ сразу");
        };
        assert_eq!(reply.request_id, 7);
        assert!(matches!(reply.result, Some(Response_::Result::Error(_))));
    }

    /// Исход — `ok` или имя отказа, с номером запроса.
    #[test]
    fn an_outcome_becomes_a_reply_with_its_request_id() {
        let mut service = service(INFO);
        let ok = service.ota_outcome(3, Ok(ports::Progress::Started));
        assert_eq!(ok.request_id, 3);
        assert!(matches!(ok.result, Some(Response_::Result::Ok(_))));
        let refused = service.ota_outcome(4, Err(ports::Rejection::NoSession));
        assert!(
            matches!(&refused.result, Some(Response_::Result::Error(e)) if e.text == "NoSession"),
            "{refused:?}"
        );
    }
    // {%- endif %}

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_chip_name_over_the_limit_is_refused() {
        let chip: &'static str = "c".repeat(proto::CHIP_BYTES + 1).leak();
        let _service = service(DeviceInfo { chip, ..INFO });
    }
}
