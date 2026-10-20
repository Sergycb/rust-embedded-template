//! Обработчик сообщений хоста по умолчанию — над примером схемы
//! `crates-host/proto`.

use embassy_time::Instant;
use ports::HostService;
// {%- if ota == "true" %}
use core::array::TryFromSliceError;
use core::fmt::Write as _;
use ports::{Chunk, OtaCommand, OtaReply, SIGNATURE_BYTES};
use supervisor::Requester;
// {%- endif %}
use proto::{Error, Info, Request, Request_, Response, Response_, heapless};

/// Сведения об устройстве, которыми обработчик отвечает на `info`.
///
/// Приходят снаружи: версию и коммит знает прошивка (`app`), чип — плата
/// (`bsp::ChipSpec`), а не `domain`.
#[derive(Debug, Clone, Copy)]
pub struct DeviceInfo {
    /// Версия прошивки, не длиннее `proto::VERSION_BYTES`.
    pub version: &'static str,
    /// Короткий хеш коммита, не длиннее `proto::COMMIT_BYTES`.
    pub commit: &'static str,
    /// Имя чипа, не длиннее `proto::CHIP_BYTES`.
    pub chip: &'static str,
    /// Заводской номер экземпляра чипа.
    pub uid: [u8; proto::UID_BYTES],
    /// Объём flash чипа, байт.
    pub flash_size: u32,
    /// Частота ядра, Гц; `None` — неизвестна.
    pub sys_hz: Option<u32>,
}

/// Обработчик, с которым узел `HOST` стоит в графе шаблона: на `info` —
/// сведения об устройстве{% if ota == "true" %}, команды обновления — узлу OTA, их исход — `ok` или
/// `error`{% endif %}, `request_id` — обратно.
///
/// Это отправная точка, а не готовая логика: проект меняет схему в
/// `crates-host/proto` и команды здесь — перевод сообщений схемы в свои
/// проверенные типы и ответы на них.
pub struct Service {
    info: DeviceInfo,
    // {%- if ota == "true" %}
    /// Клиент узла OTA: команда обновления уходит туда, исход — хосту.
    ota: Requester<'static, OtaCommand, OtaReply>,
    // {%- endif %}
}

// Вручную: у клиента узла OTA нет `Debug`.
impl core::fmt::Debug for Service {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Service")
            .field("info", &self.info)
            .finish_non_exhaustive()
    }
}

impl Service {
    /// Обработчик, отвечающий на `info` сведениями `info`{% if ota == "true" %}; команды обновления
    /// уходят узлу OTA через `ota` — клиента его входа `CMD` (`ota_cmd_requester()`
    /// графа){% endif %}.
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
        ota: Requester<'static, OtaCommand, OtaReply>,
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
        Self {
            info,
            // {%- if ota == "true" %}
            ota,
            // {%- endif %}
        }
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
        }
    }
}

impl HostService for Service {
    type Request = Request;
    type Response = Response;

    async fn handle(&mut self, request: Request) -> Response {
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
            Some(Request_::Command::OtaChunk(chunk)) => match Chunk::new(chunk.offset, &chunk.data)
            {
                Some(chunk) => self.ota(OtaCommand::Chunk(chunk)).await,
                // Ёмкость поля в схеме та же, что у куска: сюда не доходит.
                None => error("chunk too long"),
            },
            Some(Request_::Command::OtaFinish(_)) => self.ota(OtaCommand::Finish).await,
            Some(Request_::Command::Reboot(_)) => self.ota(OtaCommand::Reboot).await,
            // {%- endif %}
            None => error("unknown command"),
        };
        Response {
            request_id: request.request_id,
            result: Some(result),
        }
    }

    fn malformed(&mut self) -> Response {
        Response {
            request_id: 0,
            result: Some(error("bad frame")),
        }
    }
}
// {%- if ota == "true" %}

impl Service {
    /// Команда узлу OTA и её исход — ответом хосту.
    async fn ota(&self, command: OtaCommand) -> Response_::Result {
        match self.ota.request(command).await {
            Ok(reply) => ota_reply(reply),
            // Узел OTA не ответил — граф его потерял; хосту текстом, как
            // любой другой отказ.
            Err(_) => error("ota node lost"),
        }
    }
}

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

    /// Обработчик со сведениями `info`{% if ota == "true" %} и клиентом узла OTA, которого
    /// никто не обслуживает: тесты, которым узел нужен, берут `crate::ota::tests::live`{% endif %}.
    pub(crate) fn service(info: DeviceInfo) -> Service {
        Service::new(
            info,
            // {%- if ota == "true" %}
            crate::ota::tests::live().0,
            // {%- endif %}
        )
    }

    /// Сведения, которыми тесты собирают обработчик.
    pub(crate) const INFO: DeviceInfo = DeviceInfo {
        version: "1.2.3",
        commit: "abcdef12",
        chip: "stm32f407ve",
        uid: [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12],
        flash_size: 512 * 1024,
        sys_hz: None,
    };

    #[test]
    fn info_carries_every_field() {
        let info = service(INFO).info();
        assert_eq!(info.version, "1.2.3");
        assert_eq!(info.commit, "abcdef12");
        assert_eq!(info.chip, "stm32f407ve");
        assert_eq!(info.uid.as_slice(), &INFO.uid);
        assert_eq!(info.flash_size, 512 * 1024);
        assert_eq!(info.sys_hz, 0, "неизвестная частота — 0");
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
    /// Запрос к обработчику, пока рядом крутится узел OTA: оба future по
    /// очереди, пока обработчик не ответит.
    fn ask(
        service: &mut Service,
        node: &mut crate::ota::tests::Node,
        command: Request_::Command,
    ) -> Response_::Result {
        use core::task::{Context, Poll, Waker};
        let mut cx = Context::from_waker(Waker::noop());
        let request = Request {
            request_id: 7,
            command: Some(command),
        };
        let mut reply = core::pin::pin!(service.handle(request));
        for _ in 0..16 {
            if let Poll::Ready(response) = reply.as_mut().poll(&mut cx) {
                assert_eq!(response.request_id, 7, "request_id — обратно");
                return response.result.expect("ответ с результатом");
            }
            assert!(
                node.as_mut().poll(&mut cx).is_pending(),
                "узел OTA не выходит"
            );
        }
        panic!("обработчик не ответил");
    }

    /// Исход команды — `ok` или текст ошибки.
    fn outcome(result: &Response_::Result) -> Result<(), &str> {
        match result {
            Response_::Result::Ok(_) => Ok(()),
            Response_::Result::Error(error) => Err(&error.text),
            info @ Response_::Result::Info(_) => panic!("ожидался ok или error, пришло {info:?}"),
        }
    }

    /// Команды обновления доходят до узла OTA, исход каждой — до хоста.
    #[test]
    fn ota_commands_reach_the_node_and_each_reply_reaches_the_host() {
        let (ota, mut node) = crate::ota::tests::live();
        let mut service = Service::new(INFO, ota);

        let start = ask(
            &mut service,
            &mut node,
            Request_::Command::OtaStart(proto::OtaStart {
                len: 8,
                signature: heapless::Vec::new(),
            }),
        );
        let chunk = ask(
            &mut service,
            &mut node,
            Request_::Command::OtaChunk(proto::OtaChunk {
                data: heapless::Vec::from_slice(&[1; 8]).unwrap(),
                offset: 0,
            }),
        );
        let finish = ask(
            &mut service,
            &mut node,
            Request_::Command::OtaFinish(proto::OtaFinish {}),
        );
        let again = ask(
            &mut service,
            &mut node,
            Request_::Command::OtaFinish(proto::OtaFinish {}),
        );

        assert_eq!(outcome(&start), Ok(()));
        assert_eq!(outcome(&chunk), Ok(()));
        assert_eq!(outcome(&finish), Ok(()));
        assert_eq!(outcome(&again), Err("NoSession"));
    }

    /// Подпись не той длины — отказ текстом, до узла OTA он не доходит.
    #[test]
    fn a_signature_of_the_wrong_length_is_refused() {
        let (ota, mut node) = crate::ota::tests::live();
        let mut service = Service::new(INFO, ota);

        let result = ask(
            &mut service,
            &mut node,
            Request_::Command::OtaStart(proto::OtaStart {
                len: 8,
                signature: heapless::Vec::from_slice(&[0; 8]).unwrap(),
            }),
        );

        assert!(matches!(result, Response_::Result::Error(_)), "{result:?}");
    }
    // {%- endif %}

    #[test]
    #[should_panic(expected = "не помещается")]
    fn a_chip_name_over_the_limit_is_refused() {
        let chip: &'static str = "c".repeat(proto::CHIP_BYTES + 1).leak();
        let _service = service(DeviceInfo { chip, ..INFO });
    }
}
