//! Маршруты REST API и то, что видят их обработчики: состояние роутера и
//! тело ответа текстом.

use embassy_net::Stack;
use picoserve::extract::State;
use picoserve::io::Write;
use picoserve::response::{Content, StatusCode};
use picoserve::routing::get;
// {%- if ota == "true" %}
use picoserve::routing::post;
// {%- endif %}

use crate::device::DeviceInfo;
// {%- if ota == "true" %}
use crate::ota::commands::Progress;
use crate::ota::{Client, Command};
// {%- endif %}

use super::auth::Auth;

/// То, что видят обработчики запросов: состояние роутера.
#[derive(Clone, Copy)]
pub(super) struct Api {
    pub(super) info: DeviceInfo,
    pub(super) stack: Stack<'static>,
    pub(super) token: &'static str,
    // {%- if ota == "true" %}
    pub(super) ota: Client,
    // {%- endif %}
}

/// Тело `/api/info`: данные объекта и его длина, а не готовый текст.
///
/// `picoserve` передаёт тело по значению через десяток вложенных future
/// (обработчик, `write_to`, `write_response`, …), и каждая держит свою копию:
/// собранный заранее буфер на [`INFO_TEXT`](crate::device::INFO_TEXT) байт лёг
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
        crate::device::write_info(
            out,
            &self.info,
            Some(self.mac),
            self.ip.map(|ip| core::net::Ipv4Addr::from(ip.octets())),
            self.uptime_s,
        )
    }
}

impl Content for InfoBody {
    fn content_type(&self) -> &'static str {
        "text/plain; charset=utf-8"
    }

    fn content_length(&self) -> usize {
        self.len
    }

    async fn write_content<W: Write>(self, mut w: W) -> Result<(), W::Error> {
        let mut text = heapless::String::<{ crate::device::INFO_TEXT }>::new();
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

/// Маршруты REST API.
pub(super) fn router() -> picoserve::Router<impl picoserve::routing::PathRouter<Api>, Api> {
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
    // Строки сведений короткие (версия, хеш, имя чипа): текст короче буфера.
    // Не влез бы (проект удлинил строки или добавил поля) — `500`, а не
    // обрезанный текст.
    match body.write(&mut measure) {
        Ok(()) if measure.0 <= crate::device::INFO_TEXT => {
            body.len = measure.0;
            Ok(body)
        }
        _ => Err((StatusCode::INTERNAL_SERVER_ERROR, "info too long\n")),
    }
}
// {%- if ota == "true" %}

/// `POST /api/reboot`: узел OTA сбрасывает МК, ответив раньше сброса.
async fn reboot(api: &Api) -> (StatusCode, &'static str) {
    match api.ota.request(Command::Reboot).await {
        Ok(Ok(Progress::Rebooting)) => (StatusCode::ACCEPTED, "rebooting\n"),
        Ok(_) => (StatusCode::CONFLICT, "reboot refused\n"),
        Err(_) => (StatusCode::SERVICE_UNAVAILABLE, "ota node lost\n"),
    }
}
// {%- endif %}

#[cfg(test)]
mod tests {
    //! Маршрутизатор целиком: настоящий `picoserve` на стеке `embassy-net`,
    //! клиент — второй стек на том же кабеле в памяти
    //! (`adapters_hw::net::fake::cable`). Время `MockDriver` двигает только клиент,
    //! пока ждёт ответа (`ACK_DELAY`), — таймаутов `picoserve` оно не
    //! достигает, и соединение закрывает сам обмен.

    // {%- if ota == "true" %}
    use core::convert::Infallible;

    // {%- endif %}
    use adapters_hw::net::fake::cable::{DEVICE, leak, pair};
    use embassy_net::Stack;
    use embassy_net::tcp::TcpSocket;
    use embassy_time::{Duration, MockDriver};
    // {%- if ota == "true" %}
    use crate::ota::commands::{Progress, Rejection};
    use crate::ota::{Command, Reply};
    use supervisor::Server;
    // {%- endif %}

    use super::{Api, router};
    use crate::device::{DeviceInfo, UID_BYTES};
    use crate::http::{HTTP_BUFFER, PORT, TCP_RX, TCP_TX};
    // {%- if ota == "true" %}
    use crate::ota::{Client, Request};
    // {%- endif %}

    /// Отложенное подтверждение `smoltcp` по умолчанию.
    ///
    /// `picoserve` зовёт `flush` сразу после заголовков, до тела ответа, а
    /// `flush` сокета `embassy-net` ждёт, пока клиент подтвердит все
    /// отправленные байты. Клиенту отправлять нечего, и подтверждение он шлёт
    /// по таймеру — при стоящем времени никогда. Клиент, ждущий ответа,
    /// двигает время на эту паузу.
    const ACK_DELAY: Duration = Duration::from_millis(10);

    /// Токен прошивки в тестах.
    const TOKEN: &str = "s3cret";

    /// Сведения для `/api/info`.
    const INFO: DeviceInfo = DeviceInfo {
        version: "1.2.3",
        commit: "abcdef12",
        chip: "stm32f407ve",
        uid: [0x5a; UID_BYTES],
        flash_size: 512 * 1024,
        sys_hz: Some(168_000_000),
        reset: "pin",
        last_panic: None,
    };

    /// Методы, которыми тест стучится в каждый маршрут: метод без
    /// обработчика у маршрута — `405`, с обработчиком — `401` без токена.
    const METHODS: [&str; 5] = ["GET", "POST", "PUT", "DELETE", "PATCH"];

    /// Ответ сервера: код, заявленная длина тела и само тело.
    struct Response {
        status: u16,
        length: Option<usize>,
        body: Vec<u8>,
    }

    impl Response {
        fn parse(raw: &[u8]) -> Self {
            let end = raw
                .windows(4)
                .position(|w| w == b"\r\n\r\n")
                .expect("заголовки кончаются пустой строкой");
            let head = core::str::from_utf8(&raw[..end]).expect("заголовки — текст");
            let mut lines = head.split("\r\n");
            let status = lines
                .next()
                .and_then(|line| line.split(' ').nth(1))
                .and_then(|code| code.parse().ok())
                .expect("строка статуса с кодом");
            let length = lines
                .filter_map(|line| line.split_once(':'))
                .find(|(name, _)| name.eq_ignore_ascii_case("content-length"))
                .map(|(_, value)| value.trim().parse().expect("Content-Length — число"));
            Self {
                status,
                length,
                body: raw[end + 4..].to_vec(),
            }
        }
    }

    /// Пути маршрутов — из исходника [`router`]: первый строковый литерал
    /// каждого вызова `route`.
    ///
    /// Из исходника, а не списком в тесте: новый маршрут попадает в проверку
    /// токена сам, и забытый в нём `_: Auth` роняет тест, а не открывает API.
    /// Потолок приёма — путь литералом прямо в вызове: вложенные роутеры
    /// (`nest`) и пути из констант сюда не попадут.
    fn routes() -> Vec<&'static str> {
        let source = include_str!("api.rs");
        // Только код до тестов: в них самих вызов ищется строкой.
        let (code, _) = source
            .split_once("#[cfg(test)]")
            .expect("модуль тестов в api.rs");
        code.split(".route(")
            .skip(1)
            .map(|call| call.split('"').nth(1).expect("путь маршрута — литерал"))
            .collect()
    }

    /// Запрос без тела; `auth` — значение заголовка `Authorization`.
    fn request(method: &str, path: &str, auth: Option<&str>) -> String {
        let auth = auth
            .map(|value| format!("Authorization: {value}\r\n"))
            .unwrap_or_default();
        format!("{method} {path} HTTP/1.1\r\nHost: {DEVICE}\r\n{auth}\r\n")
    }

    /// Одно соединение с сервером: запрос, ответ до закрытия сервером.
    async fn exchange(host: Stack<'static>, request: &str) -> Response {
        let mut client = TcpSocket::new(host, leak([0; 1024]), leak([0; 1024]));
        client.connect((DEVICE, PORT)).await.unwrap();
        let mut rest = request.as_bytes();
        while !rest.is_empty() {
            let n = client.write(rest).await.unwrap();
            rest = &rest[n..];
        }
        client.flush().await.unwrap();
        // Сервер закрывает соединение после ответа (`Config::const_default`):
        // конец потока — конец ответа.
        let mut raw = Vec::new();
        let mut buf = [0; 256];
        loop {
            // Чтение не готово и за круг опроса остальных — время на
            // [`ACK_DELAY`] вперёд: так уходит подтверждение, которого ждёт
            // тело ответа.
            let n = loop {
                tokio::select! {
                    biased;
                    n = client.read(&mut buf) => break n.unwrap(),
                    () = tokio::task::yield_now() => MockDriver::get().advance(ACK_DELAY),
                }
            };
            if n == 0 {
                break;
            }
            raw.extend_from_slice(&buf[..n]);
        }
        client.close();
        client.flush().await.unwrap();
        Response::parse(&raw)
    }

    /// Гоняет сервер REST на стеке-устройстве и оба стека, пока идёт `test`;
    /// `test` получает стек-хост.
    async fn with_server<R>(
        // {%- if ota == "true" %}
        ota: Client,
        // {%- endif %}
        test: impl AsyncFnOnce(Stack<'static>) -> R,
    ) -> R {
        static CONFIG: picoserve::Config = picoserve::Config::const_default();
        let ((device, mut device_runner), (host, mut host_runner)) = pair();
        let app = router().with_state(Api {
            info: INFO,
            stack: device,
            token: TOKEN,
            // {%- if ota == "true" %}
            ota,
            // {%- endif %}
        });
        let server = picoserve::Server::new(&app, &CONFIG, leak([0; HTTP_BUFFER]))
            .listen_and_serve(0_usize, device, PORT, leak([0; TCP_RX]), leak([0; TCP_TX]));
        tokio::select! {
            never = device_runner.run() => never,
            never = host_runner.run() => never,
            never = server => match never {},
            result = test(host) => result,
        }
    }

    // {%- if ota == "true" %}
    /// Узел OTA, который на любую команду отвечает «сбрасываюсь».
    async fn agree(cmd: &mut Server<'static, Request, Reply>) -> Infallible {
        loop {
            let (_, served) = cmd.serve().await;
            served.respond(Ok(Progress::Rebooting));
        }
    }
    // {%- endif %}

    /// Без заголовка и с чужим токеном каждый маршрут из [`router`] каждым
    /// своим методом отвечает `401`; Content-Length — длина тела.
    #[tokio::test]
    async fn every_route_demands_the_token() {
        let routes = routes();
        assert!(
            routes.contains(&"/api/info"),
            "маршруты не нашлись в исходнике: {routes:?}"
        );
        // {%- if ota == "true" %}
        let (ota, mut cmd) = crate::ota::tests::cmd();
        // {%- endif %}
        with_server(
            // {%- if ota == "true" %}
            ota,
            // {%- endif %}
            async |host| {
                let sweep = async {
                    for path in &routes {
                        let mut refused = false;
                        for method in METHODS {
                            for auth in [None, Some("Bearer wrong"), Some("Basic s3cret")] {
                                let response = exchange(host, &request(method, path, auth)).await;
                                match response.status {
                                    401 => refused = true,
                                    405 => {}
                                    other => {
                                        panic!("{method} {path}, Authorization {auth:?}: {other}")
                                    }
                                }
                                assert_eq!(
                                    response.length,
                                    Some(response.body.len()),
                                    "{method} {path}: Content-Length"
                                );
                            }
                        }
                        assert!(refused, "{path}: ни один метод не ответил 401");
                    }
                };
                // {%- if ota == "true" %}
                // Узел OTA соглашается на всё: команда, дошедшая до него без
                // токена, даёт `202`, и тест падает на нём, а не виснет.
                let sweep = async {
                    tokio::select! {
                        () = sweep => {}
                        never = agree(&mut cmd) => match never {},
                    }
                };
                // {%- endif %}
                sweep.await;
            },
        )
        .await;
    }

    /// `GET /api/info` с токеном — `200` и текст, длина которого заявлена в
    /// Content-Length.
    #[tokio::test]
    async fn info_answers_with_the_token() {
        with_server(
            // {%- if ota == "true" %}
            crate::ota::tests::cmd().0,
            // {%- endif %}
            async |host| {
                let auth = format!("Bearer {TOKEN}");
                let response = exchange(host, &request("GET", "/api/info", Some(&auth))).await;
                assert_eq!(response.status, 200);
                assert_eq!(response.length, Some(response.body.len()));
                let body = core::str::from_utf8(&response.body).expect("info — текст");
                assert!(body.starts_with("version "), "{body}");
                assert!(body.contains(INFO.chip), "{body}");
            },
        )
        .await;
    }
    // {%- if ota == "true" %}

    /// `POST /api/reboot` отвечает исходом команды узлу OTA: `202` — сброс
    /// принят, `409` — любой другой ответ узла, `503` — узел взял команду и
    /// не ответил.
    #[tokio::test]
    async fn reboot_answers_with_the_ota_outcome() {
        let (ota, mut cmd) = crate::ota::tests::cmd();
        let cases: [(Option<Reply>, u16); 4] = [
            (Some(Ok(Progress::Rebooting)), 202),
            (Some(Ok(Progress::Ready)), 409),
            (Some(Err(Rejection::Busy)), 409),
            (None, 503),
        ];
        with_server(ota, async |host| {
            let node = async {
                for (reply, _) in &cases {
                    let (request, served) = cmd.serve().await;
                    assert_eq!(request.command, Command::Reboot);
                    // `None` — обязанность ответить брошена: узел потерян.
                    if let Some(reply) = reply {
                        served.respond(*reply);
                    }
                }
            };
            let client = async {
                let auth = format!("Bearer {TOKEN}");
                for (reply, status) in &cases {
                    let response =
                        exchange(host, &request("POST", "/api/reboot", Some(&auth))).await;
                    assert_eq!(response.status, *status, "исход узла {reply:?}");
                    assert_eq!(response.length, Some(response.body.len()));
                }
            };
            tokio::join!(node, client);
        })
        .await;
    }
    // {%- endif %}
}
