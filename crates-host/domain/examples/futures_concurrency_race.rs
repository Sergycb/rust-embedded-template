//! `race!`/`merge` для потоков поверх `Future` — то, чего нет в
//! `embassy_futures::select`/`select3`/`select4` (только 2-4 ветки, без
//! `race`/`merge` для потоков). Alloc-free tuple-режим (без фичи `alloc`).
//!
//! `cargo run -p domain --example futures_concurrency_race`

use futures_concurrency::future::Race;

async fn fast() -> &'static str {
    "fast"
}

async fn slow() -> &'static str {
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    "slow"
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let winner = (fast(), slow()).race().await;
    assert_eq!(winner, "fast");
}
