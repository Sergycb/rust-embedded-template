//! Узлы `HTTP`: REST API платы по сети.
//!
//! Каждый узел — одно TCP-соединение на порту [`PORT`](crate::http::PORT):
// {%- if graph == "true" %}
//! граф ставит их пулом (`HTTP_FRAG as HTTP[N]`), и сколько копий — столько
// {%- else %}
//! приложение заводит их пулом, и сколько копий — столько
// {%- endif %}
//! клиентов сервер держит разом. Число — решение приложения (`HTTP_CONNECTIONS`
//! в `[env]`), узел о нём не знает: копия — одно соединение. Сервер — `picoserve` поверх сокета стека `embassy-net`.
//!
//! | Запрос | Ответ |
//! |---|---|
//! | `GET /api/info` | сведения об устройстве, JSON |
// {%- if ota == "true" %}
//! | `POST /api/reboot` | перезагрузка — командой узлу OTA; ответ `202` уходит до сброса |
// {%- endif %}
//!
//! Это отправная точка: свои маршруты проект добавляет в `router()`, данные
//! берёт у своих узлов клиентами их входов, как перезагрузку — у OTA.
//!
//! Каждый запрос несёт `Authorization: Bearer <токен>`; токен задаёт прошивка
//! (`HttpWiring::token`, файл `rest-token.txt` в корне проекта). Без него или
//! с чужим — `401`. Токен идёт открытым текстом, как и всё по HTTP: от
//! случайного доступа он защищает, от того, кто слушает сеть, — нет.

use defmt_or_log::info;
use embassy_net::Stack;
use picoserve::extract::{FromRequestParts, State};
use picoserve::io::Write;
use picoserve::request::RequestParts;
use picoserve::response::{Content, StatusCode};
use picoserve::routing::get;
// {%- if ota == "true" %}
use picoserve::routing::post;
use ports::{OtaCommand, Progress};
// {%- endif %}
use supervisor::runtime::TaskExit;

use crate::device::DeviceInfo;
// {%- if ota == "true" %}
use crate::ota::OtaClient;
// {%- endif %}

/// Порт HTTP.
pub const PORT: u16 = 80;

/// Буфер приёма сокета, байт: полный сегмент Ethernet.
pub const TCP_RX: usize = 1536;

/// Буфер передачи сокета, байт.
pub const TCP_TX: usize = 1536;

/// Буфер разбора запроса `picoserve`, байт: строка запроса и заголовки
/// целиком, тело читается потоком.
pub const HTTP_BUFFER: usize = 2048;

/// То, что видят обработчики запросов: состояние роутера.
#[derive(Clone, Copy)]
struct Api {
    info: DeviceInfo,
    stack: Stack<'static>,
    token: &'static str,
    // {%- if ota == "true" %}
    ota: OtaClient,
    // {%- endif %}
}

/// Запрос с верным токеном.
struct Auth;

impl<'r> FromRequestParts<'r, Api> for Auth {
    type Rejection = (StatusCode, &'static str);

    #[expect(
        clippy::unused_async_trait_impl,
        reason = "сигнатуру задаёт трейт `picoserve`; проверка токена ничего не ждёт"
    )]
    async fn from_request_parts(
        api: &'r Api,
        parts: &RequestParts<'r>,
    ) -> Result<Self, Self::Rejection> {
        let header = parts.headers().get("authorization").map(|v| v.as_raw());
        if authorized(header, api.token) {
            Ok(Self)
        } else {
            Err((StatusCode::UNAUTHORIZED, "unauthorized\n"))
        }
    }
}

/// Тело `/api/info`: данные объекта и его длина, а не готовый JSON.
///
/// `picoserve` передаёт тело по значению через десяток вложенных future
/// (обработчик, `write_to`, `write_response`, …), и каждая держит свою копию:
/// собранный заранее буфер на [`INFO_MAX`](crate::device::INFO_MAX) байт лёг
/// бы в задачу узла столько же раз. Буфер заводит только `write_content` —
/// последнее звено цепочки.
struct InfoBody {
    info: DeviceInfo,
    mac: [u8; 6],
    ip: Option<embassy_net::Ipv4Address>,
    uptime_s: u64,
    len: usize,
}

impl InfoBody {
    fn write(&self, out: &mut impl core::fmt::Write) -> core::fmt::Result {
        crate::device::write_info(out, &self.info, self.mac, self.ip, self.uptime_s)
    }
}

impl Content for InfoBody {
    fn content_type(&self) -> &'static str {
        "application/json"
    }

    fn content_length(&self) -> usize {
        self.len
    }

    async fn write_content<W: Write>(self, mut w: W) -> Result<(), W::Error> {
        let mut text = heapless::String::<{ crate::device::INFO_MAX }>::new();
        // Влезает: `info` измерил этот же объект и длиннее буфера не пропустил.
        let written = self.write(&mut text);
        debug_assert!(written.is_ok(), "сведения длиннее измеренного");
        w.write_all(text.as_bytes()).await
    }
}

/// `fmt::Write`, который только считает байты.
struct Measure(usize);

impl core::fmt::Write for Measure {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        self.0 += s.len();
        Ok(())
    }
}

#[fsm::typestate]
impl HttpCtx<'_> {
    fsm::machine! {
        machine Http mod edges {
            states {
                /// Сервер слушает порт.
                Serving,
                /// Недостижимо: сервер не возвращается. Состояние нужно
                /// автомату — без выхода задача не вернула бы `TaskExit`.
                exit Down { result: TaskExit },
            }
            start => begin -> Serving;
            transitions {
                Serving => serve -> Down,
            }
        }
    }

    fn begin(&mut self) -> Serving {
        info!("domain: узел HTTP {} запущен", self.index);
        Serving
    }

    /// Принимает соединение, отвечает на его запросы, ждёт следующее —
    /// бесконечно. Отказ соединения сервер пишет в лог и слушает дальше.
    async fn serve(&mut self, _: Serving) -> Down {
        // Соединение держится между запросами: клиентов столько же, сколько
        // узлов, и один не займёт сервер навсегда — простой закрывает таймаут.
        static CONFIG: picoserve::Config =
            picoserve::Config::const_default().keep_connection_alive();
        let api = Api {
            info: *self.info,
            stack: *self.stack,
            token: self.token,
            // {%- if ota == "true" %}
            ota: *self.ota,
            // {%- endif %}
        };
        let app = router().with_state(api);
        picoserve::Server::new(&app, &CONFIG, &mut self.http[..])
            .listen_and_serve(
                self.index,
                api.stack,
                PORT,
                &mut self.rx[..],
                &mut self.tx[..],
            )
            .await
            .into_never()
    }
}

/// `Authorization: Bearer <токен>` ровно с токеном прошивки.
///
/// Схема — без учёта регистра (RFC 7235, §2.1), токен — байт в байт, а не
/// через `HeaderValue == &str`: тот регистра не различает. Сравнение — без
/// выхода на первом несовпавшем байте, чтобы время ответа не подсказывало
/// токен по байту. Пустой токен не пускает никого: иначе `Bearer ` без
/// токена открывал бы API.
fn authorized(header: Option<&[u8]>, token: &str) -> bool {
    const SCHEME: &[u8] = b"Bearer ";
    let Some(header) = header else { return false };
    let (scheme, given) = header.split_at(header.len().min(SCHEME.len()));
    let token = token.as_bytes();
    !token.is_empty()
        && scheme.eq_ignore_ascii_case(SCHEME)
        && given.len() == token.len()
        && given
            .iter()
            .zip(token)
            .fold(0, |diff, (a, b)| diff | (a ^ b))
            == 0
}

fn info(api: &Api) -> Result<InfoBody, (StatusCode, &'static str)> {
    // Среда в стеке одна — Ethernet (`medium-ethernet`).
    let embassy_net::HardwareAddress::Ethernet(mac) = api.stack.hardware_address();
    let mut body = InfoBody {
        info: api.info,
        mac: mac.0,
        ip: api.stack.config_v4().map(|config| config.address.address()),
        uptime_s: embassy_time::Instant::now().as_secs(),
        len: 0,
    };
    let mut measure = Measure(0);
    // Строки сведений короткие (версия, хеш, имя чипа): объект короче буфера.
    // Не влез бы (проект удлинил строки или добавил поля) — `500`, а не
    // обрезанный JSON.
    match body.write(&mut measure) {
        Ok(()) if measure.0 <= crate::device::INFO_MAX => {
            body.len = measure.0;
            Ok(body)
        }
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, "info too long\n")),
    }
}
// {%- if ota == "true" %}

/// `POST /api/reboot`: узел OTA сбрасывает МК, ответив раньше сброса.
async fn reboot(api: &Api) -> (StatusCode, &'static str) {
    match api.ota.request(OtaCommand::Reboot).await {
        Ok(Ok(Progress::Rebooting)) => (StatusCode::ACCEPTED, "rebooting\n"),
        Ok(_) => (StatusCode::CONFLICT, "reboot refused\n"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "ota node lost\n"),
    }
}
// {%- endif %}

/// Маршруты REST API.
fn router() -> picoserve::Router<impl picoserve::routing::PathRouter<Api>, Api> {
    // Маршрут — отдельным `let`, а не звеном одной цепочки: набор маршрутов
    // зависит от ответов генерации, и цепочка из одного звена и из двух
    // rustfmt раскладывает по-разному.
    let router = picoserve::Router::new();
    let router = router.route(
        "/api/info",
        get(async |_: Auth, State(api): State<Api>| info(&api)),
    );
    // {%- if ota == "true" %}
    let router = router.route(
        "/api/reboot",
        post(async |_: Auth, State(api): State<Api>| reboot(&api).await),
    );
    // {%- endif %}
    router
}

/// Узел `HTTP`: обслуживает соединение за соединением, пока жива прошивка;
/// не возвращается.
pub async fn task(mut ctx: HttpCtx<'_>) -> TaskExit {
    ctx.run().await.result
}

// Узел объявлен рядом со своей задачей — как остальные.
// {%- if graph == "true" %}
// Пул: граф ставит
// фрагмент `as HTTP[N]`, у каждой копии свои буферы и сокет, вход — свой
// `HttpWiring` (`[_; N]` на compose-site).
// {%- endif %}
// Буферы (5 KiB на копию) — слоты
// `= static`: статик живёт в `.bss`, и через стек они не проходят.
// `local` у стека — `embassy_net::Stack` не `Send`. Без `watchdog:`: сервер
// ждёт клиента, которого может не быть всю жизнь платы, — отмечаться ему
// нечем, как узлу `NET`.
supervisor::supervisor_fragment! {
    name: HTTP_FRAG;
    boot: inputs: struct $crate::http::HttpWiring;

    node deps: [], restart: Never,
        resources: [
            /// Сетевой стек, на котором узел открывает свой сокет.
            STACK: local $crate::net::Stack<'static> = inputs.stack,
            /// Сведения для `/api/info`.
            INFO: $crate::device::DeviceInfo = inputs.info,
            /// Токен `Authorization: Bearer`.
            TOKEN: &'static str = inputs.token,
            // {%- if ota == "true" %}
            /// Клиент входа `CMD` узла OTA — для `/api/reboot`.
            OTA: $crate::ota::OtaClient = inputs.ota,
            // {%- endif %}
            /// Приём сокета.
            RX: &'static mut [u8; $crate::http::TCP_RX] = static,
            /// Передача сокета.
            TX: &'static mut [u8; $crate::http::TCP_TX] = static,
            /// Разбор запроса.
            HTTP: &'static mut [u8; $crate::http::HTTP_BUFFER] = static
        ],
        ctx: $crate::http::HttpCtx,
        task: $crate::http::task;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_the_exact_bearer_token_passes() {
        assert!(authorized(Some(b"Bearer s3cret"), "s3cret"));
        // Схема — без учёта регистра, токен — с ним.
        assert!(authorized(Some(b"bearer s3cret"), "s3cret"));
        for header in [
            &b"Bearer S3CRET"[..],
            b"Bearer s3cret ",
            b"Bearer s3cre",
            b"s3cret",
            b"Bearer ",
            b"Basic s3cret",
        ] {
            assert!(!authorized(Some(header), "s3cret"), "{header:?}");
        }
        assert!(!authorized(None, "s3cret"));
    }

    /// Пустой токен прошивки — не «пускать всех»: `Bearer ` без токена не
    /// проходит.
    #[test]
    fn an_empty_token_lets_no_one_in() {
        for header in [&b"Bearer "[..], b"Bearer", b""] {
            assert!(!authorized(Some(header), ""), "{header:?}");
        }
    }
}
