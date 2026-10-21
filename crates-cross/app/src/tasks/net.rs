//! Узел `NET`: фоновая работа стека `embassy-net`.

#[embassy_executor::task]
async fn task(mut runner: domain::net::Runner<'static, bsp::net::Device>) {
    domain::net::task(domain::net::NetCtx {
        runner: &mut runner,
        index: 0,
    })
    .await;
}

/// Спавнит узел.
pub fn spawn(
    spawner: embassy_executor::Spawner,
    runner: domain::net::Runner<'static, bsp::net::Device>,
) {
    spawner.spawn(task(runner).expect("задача NET свежая: спавнится один раз"));
}
