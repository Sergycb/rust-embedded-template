//! Узлы `HTTP`: REST API платы по сети.
//!
//! Каждый узел — одно TCP-соединение на порту [`PORT`](crate::http::PORT):
//! граф ставит их пулом (`HTTP_FRAG as HTTP[N]`), и копий столько, сколько
//! клиентов сервер держит разом. Число — решение приложения
//! (`HTTP_CONNECTIONS` в `[env]`), узел о нём не знает. Сервер — `picoserve`
//! поверх сокета стека `embassy-net`.
//!
//! | Запрос | Ответ |
//! |---|---|
//! | `GET /api/info` | сведения об устройстве, текст |
// {%- if ota == "true" %}
//! | `POST /api/reboot` | перезагрузка — командой узлу OTA; ответ `202` уходит до сброса |
// {%- endif %}
//!
//! Это отправная точка: свои маршруты проект добавляет в `router()`, данные
//! берёт у своих узлов клиентами их входов{% if ota == "true" %}, как перезагрузку — у OTA{% endif %}.
//!
//! Каждый запрос несёт `Authorization: Bearer <токен>`; токен задаёт прошивка
//! (`HttpWiring::token`, файл `rest-token.txt` в корне проекта). Без него или
//! с чужим — `401`. Токен идёт открытым текстом, как и всё по HTTP: от
//! случайного доступа он защищает, от того, кто слушает сеть, — нет.

mod api;
mod auth;
mod config;

pub use config::{HTTP_BUFFER, PORT, TCP_RX, TCP_TX};

use defmt_or_log::info;

use api::{Api, router};

// Узел объявлен рядом со своей задачей — как остальные.
// Пул: граф ставит
// фрагмент `as HTTP[N]`, у каждой копии свои буферы и сокет, вход — свой
// `HttpWiring` (`[_; N]` на compose-site).
// Буферы (5 KiB на копию) — слоты
// `= static`: статик живёт в `.bss`, и через стек они не проходят.
// `local` у стека — `embassy_net::Stack` не `Send`. Без `watchdog:`: сервер
// ждёт клиента, которого может не быть всю жизнь платы, — отмечаться ему
// нечем, как узлу `NET`.
supervisor::supervisor_fragment! {
    name: HTTP_FRAG;
    boot: inputs: struct $crate::http::HttpWiring;

    node resources: [
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

/// Узел `HTTP`: обслуживает соединение за соединением, пока жива прошивка;
/// не возвращается.
///
/// `listen_and_serve` не отдаёт управление наружу, поэтому узел — ровно
/// «сообщить о старте и отвечать на запросы»: ни состояний, ни цикла.
/// Отказ соединения сервер пишет в лог и слушает дальше.
#[expect(
    clippy::semicolon_if_nothing_returned,
    reason = "`into_never()` возвращает `!`: хвостовая позиция — способ отдать его как `()`"
)]
pub async fn task(ctx: HttpCtx<'_>) {
    // Соединение держится между запросами: клиентов столько же, сколько
    // узлов, и один не займёт сервер навсегда — простой закрывает таймаут.
    static CONFIG: picoserve::Config = picoserve::Config::const_default().keep_connection_alive();
    info!("http: узел запущен, номер в пуле {}", ctx.index);
    let api = Api {
        info: *ctx.info,
        stack: *ctx.stack,
        token: ctx.token,
        // {%- if ota == "true" %}
        ota: *ctx.ota,
        // {%- endif %}
    };
    let app = router().with_state(api);
    picoserve::Server::new(&app, &CONFIG, &mut ctx.http[..])
        .listen_and_serve(ctx.index, api.stack, PORT, &mut ctx.rx[..], &mut ctx.tx[..])
        .await
        .into_never()
}

#[cfg(test)]
mod tests {
    //! Тесты доступа к REST API: проверка токена.

    use super::auth::authorized;

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
