//! `aselect!` — как `embassy_futures::select`, но непроигравшие ветки
//! гарантированно НЕ отменяются на середине (cancellation-safety), в отличие
//! от tokio/embassy `select!`. no_std, zero-alloc, реализует `Stream`:
//! каждый цикл арма сам решает, отдавать ли значение (`Some`) или просто
//! продолжать молча (`None`) — цикл сам по себе бесконечен, поэтому здесь
//! ограничен снаружи через `timeout`.
//!
//! `cargo run -p domain --example aselect_stream`

use aselect::aselect;
use core::pin::pin;
use futures::StreamExt;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let ticks = 0u32;
    let mut stream = pin!(aselect!(
        {
            mutable(ticks);
        },
        tick(
            { tokio::time::sleep(std::time::Duration::from_millis(2)) },
            async |fut| {
                fut.await;
            },
            |_result| {
                *ticks += 1;
                Some(*ticks)
            }
        ),
    ));

    let mut last = 0;
    // Таймаут тут ожидаем всегда (цикл арма бесконечен) — явно матчим оба
    // варианта вместо `let _ = ...`, а не тихо игнорируем `must_use`.
    match tokio::time::timeout(std::time::Duration::from_millis(50), async {
        while let Some(value) = stream.next().await {
            last = value;
        }
    })
    .await
    {
        Ok(()) | Err(_) => {}
    }

    assert!(last >= 3, "expected at least 3 ticks in 50ms, got {last}");
}
