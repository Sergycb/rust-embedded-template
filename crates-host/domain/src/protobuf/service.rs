//! Обработчик сообщений хоста по умолчанию — над примером схемы
//! `crates-host/proto`.

use embassy_time::Instant;
use micropb::{DecodeError, MessageEncode, PbEncoder};
use proto::{Get, Info, Request, Response, Response_, heapless};

use crate::device::DeviceInfo;
// {%- if ota == "true" %}
use core::array::TryFromSliceError;
use core::fmt::Write as _;

use defmt_or_log::warn;
use proto::{Error, Ota, Ota_};

use crate::ota::commands::{CHUNK_BYTES, Chunk, Rejection, SIGNATURE_BYTES};
use crate::ota::{Client, Command, Reply};
// {%- endif %}

/// Самый длинный запрос схемы, байт: под него `app` заводит буфер канала
/// узла (привязка узла в графе или задача протокола в `crates-cross/app`).
/// Предел выводит `micropb` из ёмкостей полей (`crates-host/proto/build.rs`),
/// поэтому буфер растёт вместе со схемой.
pub const REQUEST_MAX: usize = match <Request as micropb::MessageEncode>::MAX_SIZE {
    Ok(max) => max,
    Err(_) => panic!("у запроса схемы нет предела длины"),
};

/// Самый длинный ответ схемы, байт: под него `app` заводит буфер ответа узла.
pub const RESPONSE_MAX: usize = match <Response as micropb::MessageEncode>::MAX_SIZE {
    Ok(max) => max,
    Err(_) => panic!("у ответа схемы нет предела длины"),
};

/// Обработчик сообщений узла `PROTOBUF`: общий `Handlers` схемы разводит
/// запрос по обработчикам подсистем, а `request_id` возвращается в ответе.
///
/// Каждая подсистема знает свои команды и обрабатывает их здесь же:
/// `InfoHandler` — сведения, `OtaHandler` — обновление.
/// `match` по командам генерирует `crates-host/proto/build.rs` из объявлений
/// `service` в `.proto`; здесь — только `Handlers` и ответ.
///
/// Команда, которой нужно что-то от платы, получает это полем — портом
/// (`ports::…`) над адаптером, а не железом: тогда её ответ проверяется на
/// хосте заглушкой порта.
#[derive(Debug)]
pub struct Service {
    handlers: proto::host::Handlers<
        InfoHandler,
        // {%- if ota == "true" %}
        OtaHandler,
        // {%- endif %}
    >,
}

impl Service {
    /// Обработчик, отвечающий на `info` сведениями `info`{% if ota == "true" %}, а команды
    /// обновления отдающий узлу OTA клиентом `ota`{% endif %}.
    ///
    /// # Panics
    ///
    /// Если строка `info` длиннее ёмкости своего поля в схеме (`proto::*_BYTES`):
    /// обрезанная выглядела бы как настоящая, а пустую ответ не заслуживает. В
    /// `const`-контексте — ошибка компиляции.
    #[must_use]
    pub const fn new(
        info: DeviceInfo,
        // {%- if ota == "true" %}
        ota: Client,
        // {%- endif %}
    ) -> Self {
        Self {
            handlers: proto::host::Handlers {
                info: InfoHandler::new(info),
                // {%- if ota == "true" %}
                ota: OtaHandler::new(ota),
                // {%- endif %}
            },
        }
    }

    /// Выполняет запрос и строит ответ с его `request_id`.
    pub async fn handle(&mut self, request: Request) -> Response {
        let request_id = request.request_id;
        let result = self
            .handlers
            .handle(&request)
            .await
            .unwrap_or_else(|| error("unknown command"));
        response(request_id, result)
    }

    /// Ответ на кадр, который не разобрался как запрос: битые байты, строка
    /// длиннее ёмкости, чужое сообщение; `request_id` взять неоткуда — 0.
    #[must_use]
    pub fn malformed(&self) -> Response {
        response(0, error("bad frame"))
    }
}

/// Обработчик команды `info`.
#[derive(Debug)]
pub(crate) struct InfoHandler {
    info: DeviceInfo,
}

impl InfoHandler {
    /// Обработчик, отвечающий на `info` сведениями `info`.
    ///
    /// # Panics
    ///
    /// Если строка `info` длиннее ёмкости своего поля в схеме (`proto::*_BYTES`):
    /// обрезанная выглядела бы как настоящая, а пустую ответ не заслуживает. В
    /// `const`-контексте — ошибка компиляции.
    #[must_use]
    pub(crate) const fn new(info: DeviceInfo) -> Self {
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
        assert!(
            info.reset.len() <= proto::RESET_BYTES,
            "причина сброса не помещается в Info.reset"
        );
        if let Some(panic) = info.last_panic {
            assert!(
                panic.len() <= proto::PANIC_BYTES,
                "паника не помещается в Info.last_panic"
            );
        }
        Self { info }
    }

    fn message(&self) -> Info {
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
            reset: text(info.reset),
            last_panic: text(info.last_panic.unwrap_or("")),
        }
    }
}

impl proto::host::HandleInfo for InfoHandler {
    // Команда не ждёт задачу, но трейт общий с теми, что ждут.
    #[allow(
        clippy::unused_async_trait_impl,
        reason = "команда не ждёт задачу, но трейт общий с теми, что ждут"
    )]
    async fn info(&mut self, _request: &Get) -> Response_::Result {
        Response_::Result::Info(self.message())
    }
}

// {%- if ota == "true" %}
// Пределы полей схемы (`proto/build.rs`) — те же, что у команды узлу OTA:
// разойдутся — кусок или подпись, прошедшие разбор, не дойдут до узла.
const _: () = assert!(
    proto::OTA_CHUNK_BYTES == CHUNK_BYTES && proto::OTA_SIGNATURE_BYTES == SIGNATURE_BYTES,
    "пределы Ota.Chunk.data/Ota.Start.signature в proto/build.rs разошлись с ota::commands"
);

/// Обработчик команд обновления.
#[derive(Debug)]
pub(crate) struct OtaHandler {
    /// Клиент узла OTA: вход `CMD` и номер транспорта.
    ota: Client,
}

impl OtaHandler {
    /// Обработчик, отдающий команды обновления узлу OTA клиентом `ota`.
    #[must_use]
    pub(crate) const fn new(ota: Client) -> Self {
        Self { ota }
    }

    /// Команда узлу OTA и её исход; узел OTA пропал — отказ устройства.
    async fn send(&self, command: Command) -> Response_::Result {
        let reply = self.ota.request(command).await.unwrap_or_else(|_lost| {
            warn!("protobuf: узел OTA не ответил");
            Err(Rejection::Device)
        });
        ota_reply(reply)
    }

    async fn start(&mut self, request: &Ota_::Start) -> Response_::Result {
        match signature(&request.signature) {
            Ok(signature) => {
                self.send(Command::Start {
                    len: request.len,
                    signature,
                })
                .await
            }
            Err(_) => error("signature must be 64 bytes"),
        }
    }

    async fn chunk(&mut self, request: &Ota_::Chunk) -> Response_::Result {
        match Chunk::new(request.offset, &request.data) {
            Some(chunk) => self.send(Command::Chunk(chunk)).await,
            // Ёмкость поля в схеме та же, что у куска: сюда не доходит.
            None => error("chunk too long"),
        }
    }

    async fn finish(&mut self) -> Response_::Result {
        self.send(Command::Finish).await
    }

    async fn reboot(&mut self) -> Response_::Result {
        self.send(Command::Reboot).await
    }
}

impl proto::host::HandleOta for OtaHandler {
    async fn ota(&mut self, request: &Ota) -> Response_::Result {
        match &request.command {
            Some(Ota_::Command::Start(request)) => self.start(request).await,
            Some(Ota_::Command::Chunk(request)) => self.chunk(request).await,
            Some(Ota_::Command::Finish(_)) => self.finish().await,
            Some(Ota_::Command::Reboot(_)) => self.reboot().await,
            None => error("unknown command"),
        }
    }
}

/// Подпись из `start`: пусто — подписи нет (узел OTA отвергнет `Start`),
/// 64 байта — подпись, иначе отказ: обрезанную подпись принять за целую нельзя.
fn signature(bytes: &[u8]) -> Result<Option<[u8; SIGNATURE_BYTES]>, TryFromSliceError> {
    if bytes.is_empty() {
        return Ok(None);
    }
    bytes.try_into().map(Some)
}

/// Исход команды обновления — `ok` или `error` с именем отказа
/// ([`Rejection`]).
fn ota_reply(reply: Reply) -> Response_::Result {
    match reply {
        Ok(_) => Response_::Result::Ok(proto::Ok {}),
        Err(rejection) => {
            let mut message = heapless::String::new();
            // Имя варианта короче `ERROR_TEXT_BYTES`; не влезло бы — пустой
            // текст, как у `text`.
            if write!(message, "{rejection:?}").is_err() {
                message.clear();
            }
            Response_::Result::Error(Error { text: message })
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
    Response_::Result::Error(proto::Error {
        text: text(message),
    })
}

/// Строка схемы из `s`; не помещается в предел поля (`build.rs` крейта
/// `proto`) — пустая: обрезанная строка выглядела бы как настоящая. Строки
/// [`DeviceInfo`] длиннее предела отсекает обработчик сведений, так что это
/// касается только текстов ошибок.
fn text<const N: usize>(s: &str) -> heapless::String<N> {
    heapless::String::try_from(s).unwrap_or_default()
}

/// Почему кадр не разобрался — строкой: у `micropb::DecodeError` нет
/// `defmt::Format`, а `Debug2Format` втянул бы в прошивку `core::fmt` ради
/// одной строки лога.
pub(super) fn decode_reason<E>(err: &DecodeError<E>) -> &'static str {
    match err {
        DecodeError::VarIntLimit => "varint длиннее 10 байт",
        DecodeError::UnexpectedEof => "кадр оборвался",
        DecodeError::Deprecation => "устаревший тип поля",
        DecodeError::UnknownWireType => "неизвестный тип поля",
        DecodeError::ZeroField => "поле с номером 0",
        DecodeError::CustomField => "поле не разобралось",
        DecodeError::Utf8 => "строка не UTF-8",
        DecodeError::Capacity => "поле не помещается в контейнер",
        DecodeError::WrongLen => "длина поля не сходится",
        DecodeError::WrongWireType => "у поля не тот тип",
        DecodeError::NestingLimit => "вложенность сообщений глубже предела",
        DecodeError::Reader(_) => "ошибка чтения",
        // Новые причины `micropb` (`DecodeError` — `#[non_exhaustive]`).
        _ => "кадр не разобрался",
    }
}

/// Кодирует `message` в начало `buf`; `None` — не поместилось.
pub(super) fn encode(message: &impl MessageEncode, buf: &mut [u8]) -> Option<usize> {
    let capacity = buf.len();
    let mut encoder = PbEncoder::new(buf);
    message.encode(&mut encoder).ok()?;
    Some(capacity - encoder.into_writer().len())
}

#[cfg(test)]
pub(crate) const INFO: DeviceInfo = DeviceInfo {
    version: "1.2.3",
    commit: "abcdef12",
    chip: "stm32f407ve",
    uid: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
    flash_size: 512 * 1024,
    sys_hz: None,
    reset: "pin",
    last_panic: None,
};

#[cfg(test)]
mod tests {
    use core::task::{Context, Poll, Waker};

    use proto::{Get, Request_, Response_};
    // {%- if ota == "true" %}
    use proto::heapless;
    // {%- endif %}

    use super::INFO;
    use super::*;
    // {%- if ota == "true" %}
    use crate::ota::commands::Progress;
    // {%- endif %}

    /// Обработчик со сведениями `info`{% if ota == "true" %}; клиент OTA — к узлу, которого
    /// никто не опрашивает (команды обновления гоняют тесты узлов){% endif %}.
    fn service(info: DeviceInfo) -> Service {
        Service::new(
            info,
            // {%- if ota == "true" %}
            crate::ota::tests::live().0,
            // {%- endif %}
        )
    }

    /// Ответ на запрос, который не ждёт задачу.
    fn reply(mut service: Service, request: Request) -> Response {
        let mut handled = core::pin::pin!(service.handle(request));
        let mut cx = Context::from_waker(Waker::noop());
        match handled.as_mut().poll(&mut cx) {
            Poll::Ready(reply) => reply,
            Poll::Pending => panic!("ответ без ожидания"),
        }
    }

    #[test]
    fn info_is_routed_to_the_info_handler() {
        let reply = reply(
            service(INFO),
            Request {
                request_id: 7,
                command: Some(Request_::Command::Info(Get::default())),
            },
        );
        assert_eq!(reply.request_id, 7);
        assert!(matches!(reply.result, Some(Response_::Result::Info(_))));
    }

    #[test]
    fn an_unknown_command_gets_an_error() {
        let reply = reply(
            service(INFO),
            Request {
                request_id: 4,
                command: None,
            },
        );
        assert_eq!(reply.request_id, 4);
        assert!(matches!(reply.result, Some(Response_::Result::Error(_))));
    }

    // {%- if ota == "true" %}
    /// Подпись не той длины — ответ-отказ сразу, до узла OTA она не доходит:
    /// узла здесь никто не опрашивает, и ответ готов с первого опроса.
    #[test]
    fn a_signature_of_the_wrong_length_is_refused() {
        let reply = reply(
            service(INFO),
            Request {
                request_id: 7,
                command: Some(Request_::Command::Ota(proto::Ota {
                    command: Some(proto::Ota_::Command::Start(proto::Ota_::Start {
                        len: 8,
                        signature: heapless::Vec::from_slice(&[0; 8]).unwrap(),
                    })),
                })),
            },
        );
        assert_eq!(reply.request_id, 7);
        assert!(matches!(reply.result, Some(Response_::Result::Error(_))));
    }

    /// Подпись не той длины — ответ-отказ сразу, до узла OTA она не доходит.
    #[test]
    fn a_signature_is_empty_or_exactly_64_bytes() {
        assert!(signature(&[]).unwrap().is_none());
        assert!(signature(&[0; SIGNATURE_BYTES]).unwrap().is_some());
        assert!(signature(&[0; 8]).is_err());
    }

    /// Исход — `ok` или имя отказа.
    #[test]
    fn an_outcome_becomes_ok_or_the_rejection_name() {
        assert!(matches!(
            ota_reply(Ok(Progress::Started)),
            Response_::Result::Ok(_)
        ));
        let refused = ota_reply(Err(Rejection::NoSession));
        assert!(
            matches!(&refused, Response_::Result::Error(e) if e.text == "NoSession"),
            "{refused:?}"
        );
    }
    // {%- endif %}
}

#[cfg(test)]
mod info_tests {
    use super::*;

    fn handler(info: DeviceInfo) -> InfoHandler {
        InfoHandler::new(info)
    }

    #[test]
    fn info_carries_every_field() {
        let info = handler(INFO).message();
        assert_eq!(info.version, "1.2.3");
        assert_eq!(info.commit, "abcdef12");
        assert_eq!(info.chip, "stm32f407ve");
        assert_eq!(info.uid.as_slice(), &INFO.uid);
        assert_eq!(info.flash_size, 512 * 1024);
        assert_eq!(info.sys_hz, 0, "неизвестная частота — 0");
        assert_eq!(info.reset, "pin");
        assert_eq!(info.last_panic, "", "паники не было — пусто");
        let info = handler(DeviceInfo {
            last_panic: Some("main.rs:7:1:\nboom"),
            ..INFO
        })
        .message();
        assert_eq!(info.last_panic, "main.rs:7:1:\nboom");
    }

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_reset_cause_over_the_limit_is_refused() {
        let reset: &'static str = "r".repeat(proto::RESET_BYTES + 1).leak();
        let _handler = handler(DeviceInfo { reset, ..INFO });
    }

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_panic_over_the_limit_is_refused() {
        let panic: &'static str = "p".repeat(proto::PANIC_BYTES + 1).leak();
        let _handler = handler(DeviceInfo {
            last_panic: Some(panic),
            ..INFO
        });
    }

    #[test]
    fn info_reports_uptime() {
        let before = handler(INFO).message().uptime_s;
        embassy_time::MockDriver::get().advance(embassy_time::Duration::from_secs(2));
        assert!(handler(INFO).message().uptime_s >= before + 2);
    }

    /// Версия ровно в предел поля уходит целиком, а не обрезанной.
    #[test]
    fn a_version_of_exactly_the_limit_is_accepted() {
        let version: &'static str = "v".repeat(proto::VERSION_BYTES).leak();
        let info = handler(DeviceInfo { version, ..INFO }).message();
        assert_eq!(info.version, version);
    }

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_version_over_the_limit_is_refused() {
        let version: &'static str = "v".repeat(proto::VERSION_BYTES + 1).leak();
        let _handler = handler(DeviceInfo { version, ..INFO });
    }

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_chip_name_over_the_limit_is_refused() {
        let chip: &'static str = "c".repeat(proto::CHIP_BYTES + 1).leak();
        let _handler = handler(DeviceInfo { chip, ..INFO });
    }
}
