#![no_std]
#![no_main]

use shadow_rs::shadow;
shadow!(build);

mod task_orchestration;

use defmt::info;
// RTT остаётся дефолтным транспортом в обоих профилях — сырой шаблон не знает,
// какой конкретный USART на плате пользователь отведёт под лог без пробника.
// Паттерн замены (UART, без RTT/пробника) для release — doc-комментарий в
// task_orchestration.rs, подключается по мере готовности платы (Board должен
// будет отдавать реальную периферию под транспорт).
use defmt_rtt as _;
// Единственный паникёр в обоих профилях. `panic-halt` (стоял здесь под
// `#[cfg(not(debug_assertions))]`) убран: без пробника печатать всё равно
// некуда, а поведение у обоих одинаковое — остановка. `panic-probe`
// печатает бэктрейс через defmt, когда пробник подключён, и упирается в
// `udf` (дальше — HardFault-хендлер `cortex-m-rt`, вечный цикл), когда нет.
// Инвариант «ровно один `#[panic_handler]` на бинарник» держится сам собой,
// без Cargo-фич и без `cfg`.
use panic_probe as _;

use embassy_executor::Spawner;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    info!("app: starting");
    let mut _board = bsp::Board::init();
}
