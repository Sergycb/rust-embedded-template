//! Маршруты REST API и то, что видят их обработчики: состояние роутера и
//! тело ответа JSON.

use embassy_net::Stack;
use picoserve::extract::State;
use picoserve::io::Write;
use picoserve::response::{Content, StatusCode};
use picoserve::routing::get;
// {%- if ota == "true" %}
use picoserve::routing::post;
use ports::{OtaCommand, Progress};
// {%- endif %}

use crate::device::DeviceInfo;
// {%- if ota == "true" %}
use crate::ota::OtaClient;
// {%- endif %}

use super::auth::Auth;

/// То, что видят обработчики запросов: состояние роутера.
#[derive(Clone, Copy)]
pub(super) struct Api {
    pub(super) info: DeviceInfo,
    pub(super) stack: Stack<'static>,
    pub(super) token: &'static str,
    // {%- if ota == "true" %}
    pub(super) ota: OtaClient,
    // {%- endif %}
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
