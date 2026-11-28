//! Тестовый образ{% if has_protobuf == "true" %} `cargo xtask test host-target`{% endif %} — запуск той же прошивки, что
//! `app` (`src/main.rs`), с другими свойствами образа ([`IMAGE`]): версия на
//! единицу больше, хосту образ называет себя меткой `test-ota` и себя не
//! подтверждает — следующий сброс его откатывает (docs/ota.md, «Тесты OTA по
//! протоколу»). Запуск повторяет `src/main.rs` шаг в шаг: почему каждый шаг
//! такой — в комментариях там, здесь — только чем образ отличается.
#![no_std]
#![no_main]

use defmt::{error, info, warn};
use embassy_executor::Spawner;
use embassy_executor::raw::Executor;
use panic_handler as _;
use static_cell::StaticCell;

/// Свойства этого образа (`domain::ota::Image`).
///
/// Версия — рабочая + 1: ровно настолько, чтобы устройство на рабочем образе
/// той же сборки приняло тестовый (защита от отката — строго «новее»), и не
/// больше. Не `u32::MAX`: подписанный ключом проекта образ «новее всего»,
/// встань он однажды насовсем, закрыл бы устройству все обычные обновления;
/// `+1` закрыл бы самое большее одну версию — docs/ota.md, «Тесты OTA по
/// протоколу».
///
/// Текст — метка `test-ota` вместо версии: по ней хост видит, что обмен
/// разделов состоялся, не читая флеш{% if has_protobuf == "true" %} (та же строка — в
/// `crates-host/host-target-tests/tests/test.rs`){% endif %}. `confirmed: true` —
/// узел OTA начинает «уже подтверждённым» и `mark_booted` не зовёт.
const IMAGE: domain::ota::Image = domain::ota::Image {
    version: match domain::ota::version::pack_str(env!("CARGO_PKG_VERSION")).checked_add(1) {
        Some(next) => next,
        None => panic!("version в Cargo.toml — уже наибольшая, тестовому образу некуда расти"),
    },
    text: "test-ota",
    confirmed: true,
};

/// Версия этого образа по имени символа{% if has_protobuf == "true" %} — для `cargo xtask test host-target`
/// (хвост образа и проверка «рабочая + 1»){% else %}, как у рабочего `app`{% endif %}; зачем `#[used]` и `no_mangle` —
/// у того же символа в `src/main.rs`.
#[used]
#[expect(unsafe_code, reason = "имя символа нужно xtask, см. src/main.rs")]
#[unsafe(no_mangle)]
pub static FW_VERSION_IN_IMAGE: u32 = IMAGE.version;

/// Исполнитель задач — свой, как у `app`.
static EXECUTOR: StaticCell<Executor> = StaticCell::new();

/// Запуск — как у `app`, плюс предупреждение в логе: это тестовый образ.
///
/// # Panics
///
/// Задача `start` уже заспавнена — `main` позвали второй раз.
#[cortex_m_rt::entry]
fn main() -> ! {
{%- if core_baseline != "true" %}
    app::fault::enable_div_0_trap();
{%- endif %}
    info!("app: запуск {} ({})", IMAGE.text, app::build::SHORT_COMMIT);
    // Тестовый образ виден с первых строк лога.
    warn!("app: ТЕСТОВЫЙ образ test-ota: себя не подтверждает, следующий сброс его откатит");

    let mut system = cpu::System;
    let reset = system.take_cause();
    info!("app: причина сброса {}", reset);
{%- if has_protobuf == "true" or net_services != "" or has_cli == "true" %}
    app::panic::set_reset(reset);
{%- endif %}

    if let Some(reason) = app::panic::take() {
        error!("app: предыдущий запуск упал: {}", reason);
    }

    // Цикл исполнителя с замером сна — тот же, что у `app`: без него узел
    // `CPU_LOAD` видел бы ноль тиков сна и показывал бы 100 %.
    let executor = EXECUTOR.init(Executor::new(usize::MAX as *mut ()));
    let spawner = executor.spawner();
    spawner.spawn(start(spawner).expect("задача start свежая: спавнится один раз"));
    domain::cpu_load::run(executor, cortex_m::asm::wfe)
}

/// Первая задача: собирает плату и спавнит узлы — как у `app`, со своими
/// свойствами образа.
#[allow(
    clippy::unused_async,
    reason = "задача embassy обязана быть async, а плата синхронна: ждать нечего"
)]
#[embassy_executor::task]
async fn start(spawner: Spawner) {
    let board = bsp::Board::new(cpu::init());
{%- if external_flash == "true" %}
    cpu::external::init(board.ext_flash_bus, app::layout::ext_flash_end());
{%- endif %}
    info!("app: {}", board.cpu_info);
{%- if graph == "true" %}
    let boot = app::Boot {
        board,
        image: IMAGE,
    };
    if let Err(err) = app::spawn_all(&spawner, boot) {
        defmt::panic!("app: spawn_all: {}", defmt::Display2Format(&err));
    }
{%- else %}
    app::spawn_all(spawner, board, IMAGE);
{%- endif %}
}
