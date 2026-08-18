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
//! * `shutdown: Cooperative` — узел не отменяют дропом посреди работы:
//!   задача получает `stop: impl Stoppable` и выходит сама, доделав то, что
//!   нельзя бросить на середине (запись во flash, транзакция на шине).
//! * `count: N` — N независимых копий узла со своими статиками; `resources:`
//!   при этом становится массивом слотов (`PROBE[0]`, а не `PROBE_0`).
//! * `executor: NAME` — узел спавнится не на общем исполнителе, а на том,
//!   чей `SpawnerSlot` объявлен в графе (прерывательный приоритет, второе
//!   ядро).
//! * `cloned:` / `shared:` — общий на весь граф объект: первый отдаёт
//!   каждому узлу собственный `Clone`, второй — `&'static` на один
//!   физический ресурс (шина под `Mutex`).
//!
//! Полный список полей и их тонкости — в doc-комментариях самого
//! `supervisor` и `supervisor-macros`; здесь только минимальный каркас.
//!
//! Время везде — `embassy_time::Duration`, единственный тип времени во всей
//! библиотеке. `core::time::Duration` из неё убран намеренно: его
//! представление `{secs, nanos}` превращает каждую конверсию в 64-битное
//! деление, которое на 32-битной цели раскрывается в вызов
//! `__aeabi_uldivmod`, тогда как `embassy_time` — это просто счётчик тиков.
//!
//! ```ignore
//! use embassy_time::Duration;
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
//!     node USART, deps: [], restart: RestartPolicy::OnFailure, backoff: backoff(),
//!         resources: [UART: UsartResources],
//!         task: usart_worker;
//!
//!     // Стартует только после того, как USART сигнализировал готовность.
//!     node APP, deps: [USART], restart: RestartPolicy::OnFailure, backoff: backoff(),
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
//! `supervisor` пользуется им внутри (блок `watchdog:` выше), а реализовать
//! под свой МК нужно ровно один трейт — `HardwareWatchdog`. Часов крейт не
//! держит вовсе: `now` он принимает параметром, и подаёт его туда сам граф
//! (`embassy_time::Instant::now()`). Раньше здесь был второй трейт, `Clock`,
//! и его убрали не ради краткости: параметр не требует ни impl'а, ни
//! типажа, принимает любую шкалу времени (например всегда живой RTC/LPTIM —
//! драйвер `embassy_time` в STOP/STANDBY встаёт, и watchdog слеп ровно
//! тогда, когда МК может не проснуться), а прежний `core::time::Duration`
//! стоил вызова `__aeabi_uldivmod` на каждой конверсии.
//!
//! Реализация привязана к железу, поэтому её место — `bsp` (туда же
//! придётся перенести саму зависимость `watchdog` из `cross/app/Cargo.toml`
//! и добавить `embassy-time`, которого у `bsp` сейчас нет); здесь она
//! показана рядом с графом только чтобы связка читалась целиком.
//!
//! Важное следствие: `Heartbeat::feed()` узел зовёт **явно**, из тела своей
//! задачи, и только когда реально продвинулся. Автоматического «задача жива,
//! раз её future опрашивают» здесь нет — иначе watchdog сторожил бы
//! исполнитель, а не полезную работу.
//!
//! ```ignore
//! use embassy_time::Duration;
//!
//! use embassy_stm32::wdg::IndependentWatchdog;
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
//!     watchdog: Iwdg, check_every: Duration::from_millis(100);
//!
//!     node APP, deps: [], restart: RestartPolicy::OnFailure, backoff: backoff(),
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
//!
//! // в main(), после инициализации HAL и до spawn_all: граф объявил под
//! // аппаратный watchdog пустой статик, заполнить его нужно ровно один раз
//! // (второй `put` — паника: watchdog сеют один раз и назад не забирают).
//! __supervisor_watchdog.put(Iwdg(IndependentWatchdog::new(p.IWDG, 2_000_000)));
//! spawn_all(&spawner).expect("узлы свежие");
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
//!
//! # OTA: чего не хватает между bootloader'ом и работающим обновлением
//!
//! Раскладка под OTA уже есть — `ACTIVE`/`DFU`/`BOOTLOADER_STATE` посчитаны по
//! секторам чипа при генерации, `cross/boot` умеет менять разделы местами. Чего
//! шаблон не пишет, так это доставки: канал (USB, UART, сеть, SD-карта) у
//! каждой платы свой, а вместе с ним и формат пакета, и проверка подписи.
//!
//! Но два вызова из `embassy-boot` стоит знать до того, как дело дойдёт до
//! канала, потому что без них схема ведёт себя не так, как кажется:
//!
//! * `write_firmware` + `mark_updated()` — записать образ в `DFU` и пометить,
//!   что на следующем сбросе bootloader должен поменять разделы местами;
//! * `mark_booted()` — **уже из нового, загрузившегося образа**. Без него
//!   `embassy-boot` считает попытку неудачной и на следующем сбросе откатывает
//!   swap: обновление отработает ровно один раз и тихо исчезнет. Звать его
//!   надо не первой строкой `main`, а тогда, когда прошивка убедилась, что
//!   действительно жива — подняла периферию, ответила на первый запрос,
//!   отработала цикл. Иначе откат не страхует ни от чего: подтверждён будет
//!   любой образ, который смог дойти до `main`.
//!
//! ```ignore
//! use core::cell::RefCell;
//!
//! use embassy_boot_stm32::{AlignedBuffer, BlockingFirmwareUpdater, FirmwareUpdaterConfig};
//! use embassy_stm32::flash::{Flash, WRITE_SIZE};
//! use embassy_sync::blocking_mutex::Mutex;
//!
//! // Тот же цельный `Flash`, что и в cross/boot/src/main.rs — по той же
//! // причине: имена банковых регионов зависят от семейства, а границы
//! // секторов `Flash` учитывает сам.
//! let flash = Mutex::new(RefCell::new(Flash::new_blocking(p.FLASH)));
//! let mut aligned = AlignedBuffer([0; WRITE_SIZE]);
//! let mut updater = BlockingFirmwareUpdater::new(
//!     FirmwareUpdaterConfig::from_linkerfile_blocking(&flash, &flash),
//!     &mut aligned.0,
//! );
//!
//! // 1. Приём: offset растёт по мере поступления кусков из своего канала.
//! updater.write_firmware(offset, chunk)?;
//! // 2. Заявка на обмен разделами при следующем сбросе.
//! updater.mark_updated()?;
//! cortex_m::peripheral::SCB::sys_reset();
//!
//! // 3. Уже в новом образе, после того как он доказал работоспособность:
//! updater.mark_booted()?;
//! ```
//!
//! # PERSIST: данные, переживающие сброс
//!
//! Если у чипа хватило RAM, генерация отрезала от её конца килобайт под регион
//! `PERSIST` и добавила в `memory.x` секцию `.persist` — `NOLOAD`, то есть
//! образ её не инициализирует, а `cortex-m-rt` не обнуляет при старте. Этого
//! достаточно, чтобы значение пережило программный сброс (но не отключение
//! питания — это RAM, а не flash):
//!
//! ```ignore
//! #[unsafe(link_section = ".persist")]
//! static mut LAST_PANIC: [u8; 128] = [0; 128];
//! ```
//!
//! Типичное применение — причина падения: свой `#[panic_handler]` пишет в этот
//! буфер, а следующая загрузка его читает и отправляет в лог. Готового
//! `panic-persist` в зависимостях нет намеренно: он сам регистрирует
//! `#[panic_handler]`, а тот в бинарнике ровно один, и отдавать его пришлось бы
//! вместо `panic-probe`, который печатает бэктрейс через defmt при подключённом
//! пробнике. Свой хендлер решает обе задачи разом — сохранить и напечатать, —
//! но что именно в нём сохранять, зависит от устройства, поэтому шаблон его не
//! пишет.
