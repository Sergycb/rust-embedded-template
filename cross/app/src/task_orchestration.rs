//! Оркестрация embassy-задач: жизненный цикл, watchdog, компонентная
//! развязка, иерархические стейтчарты.
//!
//! Примеры ниже намеренно не компилируются (`ignore`) — этот `cross`-workspace
//! не может собраться в сыром шаблоне: `embassy-stm32`/`embassy-task-watchdog`
//! завязаны на конкретный `{{chip_feature}}`, подставляемый только при
//! генерации проекта (`cargo generate`). Раскомментируйте и адаптируйте под
//! свою плату по мере необходимости.
//!
//! # `embassy-supervisor` — упорядоченный старт/стоп задач по графу зависимостей
//!
//! ```ignore
//! use embassy_supervisor::supervisor_graph;
//!
//! supervisor_graph! {
//!     usart_task -> app_task,
//! }
//! ```
//!
//! # `embassy-task-watchdog` — мультиплексирование нескольких watchdog'ов задач
//! в один аппаратный watchdog
//!
//! ```ignore
//! use embassy_task_watchdog::WatchdogManager;
//!
//! let manager = WatchdogManager::new(p.IWDG);
//! let handle = manager.register("usart_task");
//! // где-то внутри usart_task: handle.pet().await;
//! ```
//!
//! # `ector` — actor-паттерн (message-passing) для embassy-задач: fire-and-
//! forget `notify()` между акторами, каждый актор — отдельная embassy-задача.
//! `firmware-controller` (RPC + pub-sub + периодика в одном макросе)
//! функционально богаче, но сам крейт не объявляет `#![no_std]` ни при какой
//! фиче и физически не собирается на реальном ARM-таргете — проверено
//! `cargo build --target thumbv7em-none-eabihf` с полностью чистым
//! (`rm -rf target`) билдом, падает на E0463 (`std` not found). `ector`
//! реально собирается и работает — проверено на STM32F3Discovery.
//!
//! ```ignore
//! use ector::{actor, Actor, Address, Inbox};
//!
//! struct Counter(u32);
//!
//! impl Actor for Counter {
//!     type Message = u32;
//!
//!     async fn on_mount(&mut self, _: Address<Self>, mut inbox: impl Inbox<Self>) {
//!         loop {
//!             let increment = inbox.next().await;
//!             self.0 += increment;
//!         }
//!     }
//! }
//! ```
//!
//! `hsmc` (иерархический стейтчарт как отдельная задача) сюда не входит —
//! его "embassy"-фича тянет только embassy-sync/time/futures (без
//! embassy-executor), поэтому определение чарта — чистая логика без
//! привязки к железу и живёт в `domain` (см. domain/examples/), а cross
//! только спавнит задачу, вызывающую `chart.run().await`.
//!
//! # defmt-транспорт для `release` без пробника: USB и UART
//!
//! `main.rs` держит `defmt-rtt` дефолтным транспортом в обоих профилях —
//! сырой шаблон не знает, какой конкретный USART/USB плата отведёт под лог
//! без пробника (RTT его не даёт, отладочный пробник должен быть физически
//! подключён). Ниже — два паттерна на замену для `release`, когда `Board`
//! начнёт отдавать реальную периферию. **При подключении любого паттерна
//! ниже — обязательно сделайте `use defmt_rtt as _;` в `main.rs` тоже
//! `#[cfg(debug_assertions)]`** (сейчас он безусловный): если в `release`
//! останутся активны оба `#[global_logger]` разом — та же ошибка линковки,
//! что описана ниже про `defmt-bbq`, только с `defmt-rtt` в роли второго.
//!
//! **`defmt-bbq` сюда осознанно не входит** — тянет `defmt@0.3.x` (не
//! обновлялся с 2021), а весь проект на `defmt@1.1.0`. Проверено вживую
//! (в другом проекте на этом же стеке): одновременное подключение
//! `defmt-bbq` рядом с `defmt-rtt` даёт `error: Linking globals named
//! '_defmt_acquire': symbol multiply defined!` при попытке реально
//! использовать оба — `#[global_logger]` резолвится линкером по имени
//! символа, а не как rustc lang item, поэтому конфликт жёсткий. Не путать с
//! паникёром (`panic-probe`/`panic-halt`) — тот выбирается так же через
//! `#[cfg(debug_assertions)]`, но конфликта не даёт: rustc не регистрирует
//! lang item крейта, на который в исходнике вообще нет ссылки (`use ... as
//! _`), а `#[global_logger]` резолвится линковкой по имени символа — и
//! конфликтует, если два таких символа одновременно попадают в граф.
//!
//! ## USB — `defmt-embassy-usbserial`
//!
//! Готовая задача, спавнится напрямую — не нужно писать свой drain-цикл.
//! Пины `defmt@^1` (проверено — совместим с версией в этом проекте).
//! **Не проверено**: совпадает ли типаж `embassy_usb::Driver` из его
//! `embassy-usb = "0.5"` с тем, что отдаёт актуальный `embassy-stm32` для
//! вашего чипа — сырой шаблон не даёт сконструировать реальный USB-драйвер
//! без `{{chip_feature}}`, проверяйте чистой сборкой на своей плате перед
//! тем как полагаться на это в `release`.
//!
//! ```ignore
//! #[cfg(not(debug_assertions))]
//! use defmt_embassy_usbserial as _;
//!
//! #[embassy_executor::task]
//! async fn defmt_usb_task(driver: embassy_stm32::usb::Driver<'static, embassy_stm32::peripherals::USB_OTG_FS>) {
//!     let config = embassy_usb::Config::new(0xc0de, 0xcafe);
//!     defmt_embassy_usbserial::run(driver, config).await;
//! }
//!
//! // в main(), только под release:
//! spawner.must_spawn(defmt_usb_task(usb_driver));
//! ```
//!
//! ## UART / любой другой транспорт — свой `#[global_logger]` над `bbqueue`
//!
//! Единственный найденный turnkey-крейт под UART без пробника —
//! `defmt-serial` — отпадает по той же причине, что и `defmt-bbq` выше:
//! держит `defmt = "^0.3"` (проверено на актуальной 0.13.0, публикация май
//! 2026 — апгрейда на `defmt@1.x` в issue-трекере репозитория не видно, это
//! не «скоро появится», а действующее ограничение). Раз оба turnkey-варианта
//! отпадают, паттерн приходится держать своим — благо это буквально то, чем
//! был бы `defmt-bbq`/`defmt-serial`, только против актуального
//! `defmt@1.1.0`. Grant/commit API
//! `bbqueue` уже объяснён в `cross/bsp/src/buffers.rs` (там — DMA-буфер, тут
//! — очередь под лог); ниже — только то, что специфично именно для
//! `#[global_logger]`:
//!
//! ```ignore
//! use bbqueue::BBBuffer;
//! use defmt::{Encoder, Logger};
//!
//! static QUEUE: BBBuffer<1024> = BBBuffer::new();
//! static mut PRODUCER: Option<bbqueue::Producer<'static, 1024>> = None;
//! static mut ENCODER: Encoder = Encoder::new();
//!
//! #[defmt::global_logger]
//! struct UartLogger;
//!
//! unsafe impl Logger for UartLogger {
//!     fn acquire() {
//!         // критическая секция + флаг "уже захвачен" (см. AtomicBool в
//!         // штатном примере defmt-rtt), затем ENCODER.start_frame(write)
//!     }
//!     unsafe fn flush() {
//!         // не блокируемся — данные уже в очереди, drain-таск сам разберёт
//!     }
//!     unsafe fn release() {
//!         // ENCODER.end_frame(write), снять флаг "захвачен"
//!     }
//!     unsafe fn write(bytes: &[u8]) {
//!         // grant_max_remaining(bytes.len()) в PRODUCER, copy_from_slice, commit
//!     }
//! }
//!
//! // отдельная задача, только под release — DMA-запись consumer-половины
//! // очереди в USART, аналогично cross/bsp/src/buffers.rs:
//! #[embassy_executor::task]
//! async fn uart_drain_task(mut uart: embassy_stm32::usart::UartTx<'static, embassy_stm32::mode::Async>) {
//!     let mut consumer = QUEUE.try_split().unwrap().1;
//!     loop {
//!         if let Ok(grant) = consumer.read() {
//!             let len = grant.len();
//!             let _ = uart.write(&grant).await;
//!             grant.release(len);
//!         }
//!     }
//! }
//! ```
