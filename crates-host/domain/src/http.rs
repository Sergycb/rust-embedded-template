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

mod api;
mod auth;
mod config;

pub use config::{HTTP_BUFFER, PORT, TCP_RX, TCP_TX};

use defmt_or_log::info;
use supervisor::runtime::TaskExit;

use api::{Api, router};

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

/// Узел `HTTP`: обслуживает соединение за соединением, пока жива прошивка;
/// не возвращается.
pub async fn task(mut ctx: HttpCtx<'_>) -> TaskExit {
    ctx.run().await.result
}

#[cfg(test)]
mod tests;
