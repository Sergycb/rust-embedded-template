//! Узел `NET`: фоновая работа стека `embassy-net`.

/// Спавнит узел.
pub fn spawn(
    spawner: embassy_executor::Spawner,
    runner: domain::net::Runner<'static, bsp::net::Device>,
) {
    spawner.spawn(task(runner).expect("задача NET свежая: спавнится один раз"));
}

#[embassy_executor::task]
async fn task(mut runner: domain::net::Runner<'static, bsp::net::Device>) {
    domain::net::task(domain::net::NetCtx {
        runner: &mut runner,
        index: 0,
    })
    .await;
}
