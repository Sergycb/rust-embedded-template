#![no_std]
#![no_main]

use shadow_rs::shadow;
shadow!(build);

mod task_orchestration;

use defmt::info;
// RTT остаётся дефолтным транспортом в обоих профилях — сырой шаблон не знает,
// какой конкретный USART/USB на плате пользователь отведёт под лог без
// пробника. Готовый паттерн замены (USB/UART, без RTT/пробника) для release —
// doc-комментарий в task_orchestration.rs, подключается по мере готовности
// платы (Board должен будет отдавать реальную периферию под транспорт).
use defmt_rtt as _;
#[cfg(not(debug_assertions))]
use panic_halt as _;
#[cfg(debug_assertions)]
use panic_probe as _;

use embassy_executor::Spawner;

#[embassy_executor::main]
async fn main(_spawner: Spawner) {
    info!("app: starting");
    let mut _board = bsp::Board::init();
}
