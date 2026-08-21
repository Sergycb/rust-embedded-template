//! Межзадачный (не host<->target!) request/response: клиентские задачи
//! вызывают `request()`, ровно одна серверная задача отвечает через
//! `serve()`. Построен на `embassy_sync`-сигналах/мьютексах и не зависит от
//! `embassy-executor` — работает под любым исполнителем.
//!
//! Заменил `embedded-rpc`. Ключевое отличие — не API, а поведение при
//! отмене: каждый вызов помечен номером поколения, поэтому брошенный по
//! таймауту или проигравший в `select!` запрос не «протечёт» ответом в
//! следующий, ни к нему не относящийся `request()`. Ещё две вещи, которых
//! не было: право отвечать выдаётся один раз (`server()` возвращает `None`
//! на второй вызов — `&RpcService` сам по себе не позволяет притвориться
//! сервером), а `Requester` даёт клиентам ручку без доступа к `server()`.
//!
//! Таймаута из коробки нет намеренно — крейт не зависит от `embassy-time`;
//! в прошивке его добавляет `supervisor::RequestTimeoutExt` поверх этой же
//! отмены по поколениям.
//!
//! `cargo run -p domain --example sync_request_service`

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use sync_request::RpcService;

static SERVICE: RpcService<CriticalSectionRawMutex, u32, u32> = RpcService::new();

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut server = SERVICE.server().expect("сервер ещё никем не занят");

    tokio::spawn(async move {
        loop {
            let (req, served) = server.serve().await;
            served.respond(req.saturating_add(1));
        }
    });

    assert_eq!(SERVICE.request(5).await, Ok(6));
    assert_eq!(SERVICE.request(41).await, Ok(42));

    // Право отвечать уже отдано выше: вторая задача, даже имея `&SERVICE`,
    // сервером стать не может.
    assert!(SERVICE.server().is_none());

    // Клиентская ручка для тех, кому `server()` вообще не должен быть виден.
    let requester = SERVICE.requester();
    assert_eq!(requester.request(0).await, Ok(1));
}
