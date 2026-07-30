//! Оркестрация embassy-задач: жизненный цикл, рестарты, watchdog,
//! межзадачный обмен.
//!
//! Примеры ниже намеренно не компилируются (`ignore`) — этот `cross`-workspace
//! не может собраться в сыром шаблоне: `embassy-stm32` завязан на конкретный
//! `{{chip_feature}}`, подставляемый только при генерации проекта
//! (`cargo generate`). Раскомментируйте и адаптируйте под свою плату по мере
//! необходимости.
//!
//! # `supervisor` — граф задач: порядок старта, рестарты, обмен, watchdog
//!
//! Один макрос `supervisor_graph!` описывает весь набор задач сразу. Он
//! заменил три сторонних крейта, каждый из которых описывал свой срез того
//! же графа своим DSL: `embassy-supervisor` (порядок старта по зависимостям),
//! `embassy-task-watchdog` (мультиплексор watchdog'ов) и `ector` (актор с
//! почтовым ящиком). Раньше один и тот же граф приходилось объявлять трижды
//! и следить за тем, чтобы три декларации не разъехались.
//!
//! Что даёт узел графа, помимо `deps:` (упорядоченный старт — через явный
//! барьер готовности, а не порядок спавна) и `restart:`/`backoff:`
//! (политика перезапуска с backoff'ом):
//!
//! * `resources: [SLOT: Type]` — эксклюзивная передача владения (обычно
//!   ручкой периферии) в задачу на один прогон и обратно при её остановке,
//!   чтобы следующий перезапуск получил тот же объект. Отвечает на вопрос,
//!   который сам по себе `Spawner` не решает: как перезапустить задачу,
//!   забравшую `Peripherals`-ручку.
//! * `inbox: [FIELD: Type; N]` — ограниченная упорядоченная очередь входящих
//!   сообщений (это и есть бывший `ector`: fire-and-forget между задачами).
//!   Прямо стыкуется с `fsm_async::AsyncTimedRuntime::run` — см. ниже.
//! * `publish: [FIELD: Type; N]` / `subscribe: [OTHER.FIELD]` — broadcast
//!   состояния наружу через `embassy_sync::watch::Watch`.
//! * `request: ...` / `calls: [OTHER.FIELD]` — request/response поверх
//!   `sync_request::RpcService` (того самого, что в `domain`), с
//!   `RequestTimeoutExt::request_timeout` для таймаута.
//! * `watchdog: Duration` — участие узла в общем аппаратном watchdog'е,
//!   см. следующий раздел.
//!
//! Полный список полей и их тонкости — в doc-комментариях самого
//! `supervisor` и `supervisor-macros`; здесь только минимальный каркас.
//!
//! ```ignore
//! use core::time::Duration;
//!
//! use supervisor::policy::{BackoffPolicy, JitterPolicy, RestartPolicy};
//! use supervisor::runtime::TaskExit;
//! use supervisor::supervisor_graph;
//!
//! fn backoff() -> BackoffPolicy {
//!     BackoffPolicy {
//!         first: Duration::from_millis(50),
//!         factor: 2,
//!         jitter: JitterPolicy::None,
//!         floor: Duration::from_millis(50),
//!         max: Duration::from_secs(5),
//!     }
//! }
//!
//! supervisor_graph! {
//!     node USART = Terminate, deps: [], restart: RestartPolicy::OnFailure, backoff: backoff(),
//!         resources: [UART: UsartResources],
//!         task: usart_worker;
//!
//!     // Стартует только после того, как USART сигнализировал готовность.
//!     node APP = Terminate, deps: [USART], restart: RestartPolicy::OnFailure, backoff: backoff(),
//!         inbox: [EVENTS: LinkEvent; 8],
//!         task: app_worker;
//! }
//!
//! async fn app_worker(ctx: AppCtx<'_>) -> TaskExit {
//!     // Определение автомата — в `domain` (см. domain/examples/), здесь
//!     // только прогон его в задаче.
//!     let mut rt = AsyncTimedRuntime::<Link>::new(LinkId::Disconnected, LinkData::default());
//!     rt.run(ctx.events).await; // не возвращается; отменяется дропом при shutdown
//!     TaskExit::Completed
//! }
//!
//! // в main(), после инициализации HAL:
//! provide_uart(r.usart).expect("слот пуст до первого spawn_all");
//! spawn_all(&spawner).expect("узлы свежие");
//! ```
//!
//! # `watchdog` — мультиплексор задач в один аппаратный watchdog
//!
//! `supervisor` пользуется им внутри (блок `watchdog:` выше), но два трейта
//! приходится реализовать самому — крейт намеренно не знает ни про
//! `embassy-time`, ни про конкретный МК. Реализации привязаны к железу,
//! поэтому их место — `bsp` (туда же придётся перенести саму зависимость
//! `watchdog` из `cross/app/Cargo.toml` и добавить `embassy-time`, которого
//! у `bsp` сейчас нет); здесь они показаны рядом с графом только чтобы
//! связка читалась целиком.
//!
//! Важное следствие: `Heartbeat::feed()` узел зовёт **явно**, из тела своей
//! задачи, и только когда реально продвинулся. Автоматического «задача жива,
//! раз её future опрашивают» здесь нет — иначе watchdog сторожил бы
//! исполнитель, а не полезную работу.
//!
//! ```ignore
//! use core::time::Duration;
//!
//! use embassy_stm32::wdg::IndependentWatchdog;
//!
//! struct EmbassyClock;
//!
//! impl watchdog::Clock for EmbassyClock {
//!     fn now(&self) -> Duration {
//!         embassy_time::Instant::now().duration_since(embassy_time::Instant::from_ticks(0)).into()
//!     }
//! }
//!
//! struct Iwdg(IndependentWatchdog<'static, embassy_stm32::peripherals::IWDG>);
//!
//! impl watchdog::HardwareWatchdog for Iwdg {
//!     fn feed(&mut self) {
//!         self.0.pet();
//!     }
//!
//!     fn trigger_reset(&mut self) -> ! {
//!         // Перестаём кормить и ждём, пока IWDG сбросит МК.
//!         loop {
//!             cortex_m::asm::wfi();
//!         }
//!     }
//! }
//!
//! // В графе: блок уровня графа плюс opt-in у каждого узла.
//! supervisor_graph! {
//!     watchdog: EmbassyClock, Iwdg, check_every: Duration::from_millis(100);
//!
//!     node APP = Terminate, deps: [], restart: RestartPolicy::OnFailure, backoff: backoff(),
//!         watchdog: Duration::from_secs(2),
//!         task: app_worker;
//! }
//!
//! async fn app_worker(ctx: AppCtx<'_>) -> TaskExit {
//!     loop {
//!         do_one_unit_of_work().await;
//!         ctx.heartbeat.feed(); // только после реального прогресса
//!     }
//! }
//! ```
//!
//! # Стейтчарты сюда не входят
//!
//! `fsm` и `fsm-async` живут в `domain`, а не здесь: `fsm` вообще ни от чего
//! не зависит, а `fsm-async` тянет только `embassy-sync`/`time`/`futures`,
//! без `embassy-executor`. То есть определение автомата — чистая логика без
//! привязки к железу, а `cross` лишь спавнит задачу, гоняющую `run().await`,
//! и кормит её событиями через `inbox:` (см. `app_worker` выше).
//!
//! # defmt-транспорт для `release` без пробника: UART
//!
//! `main.rs` держит `defmt-rtt` дефолтным транспортом в обоих профилях —
//! сырой шаблон не знает, какой конкретный USART/USB плата отведёт под лог
//! без пробника (RTT его не даёт, отладочный пробник должен быть физически
//! подключён). Ниже — паттерн на замену для `release`, когда `Board` начнёт
//! отдавать реальную периферию. **При его подключении обязательно сделайте
//! `use defmt_rtt as _;` в `main.rs` `#[cfg(debug_assertions)]`** (сейчас он
//! безусловный): `#[global_logger]` резолвится линкером по имени символа
//! (`_defmt_acquire` и т.п.), а не как rustc lang item, поэтому два активных
//! разом дадут жёсткий `error: Linking globals named '_defmt_acquire':
//! symbol multiply defined!`. Инвариант — ровно один активный
//! `use <логгер> as _;` на сборку.
//!
//! **Готовых крейтов под это в шаблоне нет — все три проверенных отпали:**
//!
//! * `defmt-bbq` — держит `defmt@0.3.x` (не обновлялся с 2021) при
//!   `defmt@1.1.0` у нас. Проверено вживую в другом проекте на этом же
//!   стеке: рядом с `defmt-rtt` даёт ровно ту самую ошибку линковки выше.
//! * `defmt-serial` — единственный turnkey-вариант под UART, отпадает по
//!   той же причине: `defmt = "^0.3"` (проверено на актуальной 0.13.0,
//!   публикация май 2026; апгрейда на `defmt@1.x` в issue-трекере не
//!   видно — это не «скоро появится», а действующее ограничение).
//! * `defmt-embassy-usbserial` — готовый `#[global_logger]` над USB CDC-ACM,
//!   с `defmt@^1` совместим. Убран вместе с остальными заменёнными
//!   зависимостями, и возвращать его не стоит по отдельной причине: он
//!   требует `embassy-usb = "^0.5"`, из-за чего `embassy-usb` приходилось
//!   держать запиненным на 0.5 вместо парной к текущей `embassy-stm32`
//!   версии — поднять её нельзя, иначе в графе окажутся две несовместимые
//!   копии `embassy-usb`.
//!
//! Раз все turnkey-варианты отпадают, паттерн приходится держать своим —
//! благо это буквально то, чем был бы `defmt-bbq`/`defmt-serial`, только
//! против актуального `defmt@1.1.0`. Grant/commit API `bbqueue` уже
//! объяснён в `cross/bsp/src/buffers.rs` (там — DMA-буфер, тут — очередь под
//! лог); ниже — только то, что специфично именно для `#[global_logger]`:
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
