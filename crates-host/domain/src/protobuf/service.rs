//! Обработчик сообщений хоста по умолчанию — над примером схемы
//! `crates-host/proto`.

use embassy_time::Instant;
// {%- if ota == "true" %}
use core::array::TryFromSliceError;
use core::fmt::Write as _;
use defmt_or_log::warn;
use ports::{Chunk, OtaCommand, OtaReply, Rejection, SIGNATURE_BYTES};

use crate::ota::OtaClient;
// {%- endif %}
use proto::{Error, Info, Request, Request_, Response, Response_, heapless};

pub use crate::device::DeviceInfo;

// Поле `uid` схемы — ровно заводской номер: копируется без проверки длины.
const _: () = assert!(proto::UID_BYTES == crate::device::UID_BYTES);
// Прошлую панику `app` сжимает до `PANIC_BRIEF` — она помещается в поле.
const _: () = assert!(crate::device::PANIC_BRIEF <= proto::PANIC_BYTES);

/// Самый длинный запрос схемы, байт: под него `app` заводит буфер канала
/// узла (`crate::link::<вид>` в `crates-cross/app`). Предел выводит
/// `micropb` из ёмкостей полей (`crates-host/proto/build.rs`), поэтому
/// буфер растёт вместе со схемой.
pub const REQUEST_MAX: usize = match <Request as micropb::MessageEncode>::MAX_SIZE {
    Ok(max) => max,
    Err(_) => panic!("у запроса схемы нет предела длины"),
};

/// Самый длинный ответ схемы, байт: под него `app` заводит буфер ответа узла.
pub const RESPONSE_MAX: usize = match <Response as micropb::MessageEncode>::MAX_SIZE {
    Ok(max) => max,
    Err(_) => panic!("у ответа схемы нет предела длины"),
};

/// Обработчик сообщений узла `PROTOBUF`: на `info` — сведения об
/// устройстве{% if ota == "true" %}, команды обновления — узлу OTA на вход `CMD`, их исход —
/// `ok` или `error`{% endif %}, `request_id` — обратно.
///
/// Это отправная точка, а не готовая логика: проект меняет схему в
/// `crates-host/proto` и команды здесь — перевод сообщений схемы в свои
/// проверенные типы и ответы на них.
///
/// Команда, которой нужно что-то от платы, получает это полем — портом
/// (`ports::…`) над адаптером, а не железом: тогда её ответ проверяется на
/// хосте заглушкой порта. Набросок (`ports::Relay` — порт проекта, в шаблоне
/// его нет): поле `relay: R` у `Service<R: ports::Relay>`, команда
/// `Request_::Relay(on)` → `self.relay.set(on).await` → `Ok` или `Error`; узел
/// и его фрагмент получают тот же параметр.
#[derive(Debug)]
pub struct Service {
    info: DeviceInfo,
    // {%- if ota == "true" %}
    /// Клиент узла OTA: вход `CMD` и номер транспорта.
    ota: OtaClient,
    // {%- endif %}
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
        ota: OtaClient,
        // {%- endif %}
    ) -> Self {
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
        Self {
            info,
            // {%- if ota == "true" %}
            ota,
            // {%- endif %}
        }
    }

    /// Выполняет запрос и строит ответ с его `request_id`.
    ///
    /// Сообщения схемы здесь превращаются в проверенные значения своих
    /// типов: недопустимое отсекается ответом-ошибкой и дальше не уходит.
    // `allow`, а не `expect`: без OTA шаблонной команде ждать нечего, а с OTA
    // и в командах проекта, которые ждут задачу, `.await` здесь есть.
    #[allow(
        clippy::unused_async,
        clippy::unused_async_trait_impl,
        reason = "без OTA ждать нечего, но команда проекта может ждать задачу"
    )]
    pub async fn handle(&mut self, request: Request) -> Response {
        let request_id = request.request_id;
        let result = match request.command {
            Some(Request_::Command::Info(_)) => Response_::Result::Info(self.info()),
            // {%- if ota == "true" %}
            Some(Request_::Command::OtaStart(start)) => match signature(&start.signature) {
                Ok(signature) => {
                    self.ota(OtaCommand::Start {
                        len: start.len,
                        signature,
                    })
                    .await
                }
                Err(_) => error("signature must be 64 bytes"),
            },
            Some(Request_::Command::OtaChunk(chunk)) => {
                match Chunk::new(chunk.offset, &chunk.data) {
                    Some(chunk) => self.ota(OtaCommand::Chunk(chunk)).await,
                    // Ёмкость поля в схеме та же, что у куска: сюда не доходит.
                    None => error("chunk too long"),
                }
            }
            Some(Request_::Command::OtaFinish(_)) => self.ota(OtaCommand::Finish).await,
            Some(Request_::Command::Reboot(_)) => self.ota(OtaCommand::Reboot).await,
            // {%- endif %}
            None => error("unknown command"),
        };
        response(request_id, result)
    }

    /// Ответ на кадр, который не разобрался как запрос: битые байты, строка
    /// длиннее ёмкости, чужое сообщение; `request_id` взять неоткуда — 0.
    #[must_use]
    pub fn malformed(&self) -> Response {
        response(0, error("bad frame"))
    }
    // {%- if ota == "true" %}

    /// Команда узлу OTA и её исход; узел OTA пропал — отказ устройства.
    async fn ota(&self, command: OtaCommand) -> Response_::Result {
        let reply = self.ota.request(command).await.unwrap_or_else(|_lost| {
            warn!("protobuf: узел OTA не ответил");
            Err(Rejection::Device)
        });
        ota_reply(reply)
    }
    // {%- endif %}

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
            reset: text(info.reset),
            last_panic: text(info.last_panic.unwrap_or("")),
        }
    }
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
        reset: "pin",
        last_panic: None,
    };

    /// Обработчик со сведениями `info`{% if ota == "true" %}; клиент OTA — к узлу, которого
    /// никто не опрашивает (команды обновления гоняют тесты узла `PROTOBUF`){% endif %}.
    pub(crate) fn service(info: DeviceInfo) -> Service {
        Service::new(
            info,
            // {%- if ota == "true" %}
            crate::ota::tests::live().0,
            // {%- endif %}
        )
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

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_reset_cause_over_the_limit_is_refused() {
        let reset: &'static str = "r".repeat(proto::RESET_BYTES + 1).leak();
        let _service = service(DeviceInfo { reset, ..INFO });
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
    /// Подпись не той длины — ответ-отказ сразу, до узла OTA она не доходит:
    /// узла здесь никто не опрашивает, и ответ готов с первого опроса.
    #[test]
    fn a_signature_of_the_wrong_length_is_refused() {
        let mut service = service(INFO);
        let handled = core::pin::pin!(service.handle(Request {
            request_id: 7,
            command: Some(Request_::Command::OtaStart(proto::OtaStart {
                len: 8,
                signature: heapless::Vec::from_slice(&[0; 8]).unwrap(),
            })),
        }));
        let mut cx = core::task::Context::from_waker(core::task::Waker::noop());
        let core::task::Poll::Ready(reply) = handled.poll(&mut cx) else {
            panic!("отказ — без узла OTA");
        };
        assert_eq!(reply.request_id, 7);
        assert!(matches!(reply.result, Some(Response_::Result::Error(_))));
    }

    /// Исход — `ok` или имя отказа.
    #[test]
    fn an_outcome_becomes_ok_or_the_rejection_name() {
        assert!(matches!(
            ota_reply(Ok(ports::Progress::Started)),
            Response_::Result::Ok(_)
        ));
        let refused = ota_reply(Err(Rejection::NoSession));
        assert!(
            matches!(&refused, Response_::Result::Error(e) if e.text == "NoSession"),
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
