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

/// Тело ответа JSON, собранное в буфере.
struct Json(heapless::String<{ crate::device::INFO_MAX }>);

impl Content for Json {
    fn content_type(&self) -> &'static str {
        "application/json"
    }

    fn content_length(&self) -> usize {
        self.0.len()
    }

    async fn write_content<W: Write>(self, mut w: W) -> Result<(), W::Error> {
        w.write_all(self.0.as_bytes()).await
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

fn info(api: &Api) -> Result<Json, (StatusCode, &'static str)> {
    let mut text = heapless::String::new();
    // Среда в стеке одна — Ethernet (`medium-ethernet`).
    let embassy_net::HardwareAddress::Ethernet(mac) = api.stack.hardware_address();
    let ip = api.stack.config_v4().map(|config| config.address.address());
    let uptime_s = embassy_time::Instant::now().as_secs();
    // Строки сведений короткие (версия, хеш, имя чипа): объект короче буфера.
    // Не влез бы (проект удлинил строки или добавил поля) — `500`, а не
    // обрезанный JSON.
    match crate::device::write_info(&mut text, &api.info, mac.0, ip, uptime_s) {
        Ok(()) => Ok(Json(text)),
        Err(core::fmt::Error) => Err((StatusCode::INTERNAL_SERVER_ERROR, "info too long\n")),
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
