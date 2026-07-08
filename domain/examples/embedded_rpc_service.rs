//! Межзадачный (не host<->target!) request/response с zero-copy буферами
//! (`Req` может содержать `&mut [u8]`, сервер пишет прямо в память клиента —
//! не показано здесь ради простоты, но именно это отличает крейт от
//! RPC внутри `firmware-controller`, который такого не разрешает вовсе).
//! Построен на `embassy_sync`-мьютексах/wakers, таймаута из коробки нет —
//! добавляется снаружи через `embassy_time::with_timeout`.
//!
//! `cargo run -p domain --example embedded_rpc_service`

use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
use embedded_rpc::RpcService;

static SERVICE: RpcService<CriticalSectionRawMutex, u32, u32> = RpcService::new();

async fn server() {
    loop {
        let (req, served) = SERVICE.serve().await;
        served.respond(req.saturating_add(1));
    }
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    tokio::spawn(server());

    match SERVICE.request(5).await {
        Ok(n) => assert_eq!(n, 6),
        Err(_) => panic!("server dropped the request"),
    }
}
