Крейт `app`: загрузка процессора и ручной цикл исполнителя, логгер без
пробника, питание от батареи, данные, переживающие сброс.

Примеры ниже намеренно не компилируются (`ignore`) — этот `cross`-workspace
не может собраться в сыром шаблоне: `embassy-stm32` завязан на конкретный
`{{chip_feature}}`, подставляемый только при генерации проекта
(`cargo generate`). Раскомментируйте и адаптируйте под свою плату по мере
необходимости.
{%- if graph == "true" %}

# `CPU_LOAD`: загрузка процессора, и почему цикл исполнителя ручной

Второй узел шаблона, кроме `HEARTBEAT`, — `CPU_LOAD`: он меряет, во сколько
процентов занят процессор, и публикует результат в `PERCENT`. Фрагмент —
`crates-host/domain/src/cpu_load.rs`, арифметика и её host-тесты —
`crates-host/domain/src/cpu_load/load.rs`, счётчик и объяснения —
`app-cpu-load.md`.

Читать загрузку — ваше дело: у него ровно один слот подписчика, и первым его
занимает тот, кто первым позовёт `cpu_load_percent_receiver()`. Либо разово,
либо постоянно — объявив у себя `subscribe: [CPU_LOAD_PERCENT: u8]`.
Обратите внимание, чего в `fragments:` нет: `provide_cpu_load_sleep_ticks(..)`.
У узла `SLEEP_TICKS` есть инициализатор (`= inputs.sleep_ticks`) и своя
`boot:`-привязка, поэтому заполняет его сам граф, и забыть строку нельзя{% if ota == "true" %} —
ровно как у узла `OTA` (`app-graph.md`){% endif %}.
{%- else %}

# Загрузка процессора, и почему цикл исполнителя ручной

Задача `cpu_load` меряет, во сколько процентов занят процессор, и пишет
результат в лог. Арифметика и её host-тесты —
`crates-host/domain/src/cpu_load/load.rs`, счётчик и объяснения —
`app-cpu-load.md`. Спавнит её `tasks::spawn_all` — через
`#[embassy_executor::task]` в `app/src/cpu_load.rs`, потому что
`Spawner::spawn` берёт `SpawnToken`, а не функцию.
{%- endif %}

Ради одного счётчика шаблон отказался от `#[embassy_executor::main]`: у
макроса `WFE` живёт внутри `Executor::run`, между двумя вызовами `poll`, и
снаружи их не видно, а именно там проходит всё время, когда задач нет. Про
то, как выглядит ручной цикл и почему он устроен именно так, — в
`app-cpu-load.md`.

# Стейтчарты сюда не входят

Понадобится автомат состояний (например, крейт `fsm` из
[rust-lib]({{rust_lib}})) — его место в `domain`, а
не здесь, даже если он владеет своей задачей целиком и ждёт события из
`embassy_sync`-канала: сама модель состояний остаётся чистой логикой без
привязки к железу, а `cross` лишь спавнит задачу, гоняющую `run().await`{% if graph == "true" %},
и кормит её событиями через `inbox:` (`app-graph.md`){% endif %}.

# defmt-транспорт для `release` без пробника: UART

`main.rs` держит `defmt-rtt` дефолтным транспортом в обоих профилях —
сырой шаблон не знает, какой конкретный USART{% if has_usb == "true" %}/USB{% endif %} плата отведёт под лог
без пробника (RTT его не даёт, отладочный пробник должен быть физически
подключён). Ниже — паттерн на замену для `release`, когда `Board` начнёт
отдавать реальную периферию. **При его подключении обязательно сделайте
`use defmt_rtt as _;` в `main.rs` `#[cfg(debug_assertions)]`** (сейчас он
безусловный): `#[global_logger]` резолвится линкером по имени символа
(`_defmt_acquire` и т.п.), а не как rustc lang item, поэтому два активных
разом дадут жёсткий `error: Linking globals named '_defmt_acquire':
symbol multiply defined!`. Инвариант — ровно один активный
`use <логгер> as _;` на сборку.

**Готовых крейтов под это в шаблоне нет — {% if has_usb == "true" %}все три проверенных{% else %}оба проверенных{% endif %} отпали:**

* `defmt-bbq` — держит `defmt@0.3.x` (не обновлялся с 2021) при
  `defmt@1.x` у нас. Проверено вживую в другом проекте на этом же
  стеке: рядом с `defmt-rtt` даёт ровно ту самую ошибку линковки выше.
* `defmt-serial` — единственный turnkey-вариант под UART, отпадает по
  той же причине: `defmt = "^0.3"` (проверено на актуальной 0.13.0,
  публикация май 2026; апгрейда на `defmt@1.x` в issue-трекере не
  видно — это не «скоро появится», а действующее ограничение).
{%- if has_usb == "true" %}
* `defmt-embassy-usbserial` — готовый `#[global_logger]` над USB CDC-ACM,
  с `defmt@^1` совместим. Убран вместе с остальными заменёнными
  зависимостями, и возвращать его не стоит по отдельной причине: он
  требует `embassy-usb = "^0.5"`, из-за чего `embassy-usb` приходилось
  держать запиненным на 0.5 вместо парной к текущей `embassy-stm32`
  версии — поднять её нельзя, иначе в дереве зависимостей окажутся две несовместимые
  копии `embassy-usb`.
{%- endif %}

Раз все turnkey-варианты отпадают, паттерн приходится держать своим —
благо это буквально то, чем был бы `defmt-bbq`/`defmt-serial`, только
против актуального `defmt@1.x`. Grant/commit API `bbqueue` уже
объяснён в `docs/buffers.md` (там — DMA-буфер, тут — очередь под
лог); ниже — только то, что специфично именно для `#[global_logger]`:

```ignore
use bbqueue::BBBuffer;
use defmt::{Encoder, Logger};

static QUEUE: BBBuffer<1024> = BBBuffer::new();
static mut PRODUCER: Option<bbqueue::Producer<'static, 1024>> = None;
static mut ENCODER: Encoder = Encoder::new();

#[defmt::global_logger]
struct UartLogger;

unsafe impl Logger for UartLogger {
    fn acquire() {
        // критическая секция + флаг "уже захвачен" (см. AtomicBool в
        // штатном примере defmt-rtt), затем ENCODER.start_frame(write)
    }
    unsafe fn flush() {
        // не блокируемся — данные уже в очереди, drain-таск сам разберёт
    }
    unsafe fn release() {
        // ENCODER.end_frame(write), снять флаг "захвачен"
    }
    unsafe fn write(bytes: &[u8]) {
        // grant_max_remaining(bytes.len()) в PRODUCER, copy_from_slice, commit
    }
}

// отдельная задача, только под release — DMA-запись consumer-половины
// очереди в USART, аналогично примеру из docs/buffers.md:
#[embassy_executor::task]
async fn uart_drain_task(mut uart: embassy_stm32::usart::UartTx<'static, embassy_stm32::mode::Async>) {
    let mut consumer = QUEUE.try_split().unwrap().1;
    loop {
        if let Ok(grant) = consumer.read() {
            let len = grant.len();
            let _ = uart.write(&grant).await;
            grant.release(len);
        }
    }
}
```
{%- if has_modbus == "true" %}

# Modbus: одна карта на все узлы `MODBUS_*`

Узел `MODBUS_<ТРАНСПОРТ>` — по одному на транспорт с Modbus в `links`.{% if graph == "true" %}
Фрагмент — узел протокола `domain::SERVER_FRAG<L, H>`: канал
`L` (`crate::link::<вид>::<транспорт>::Link`) строит граф —
`crate::link::<вид>::<транспорт>::link` над рычагом платы в памяти `static T`, обработчик `H` —
`domain::modbus::Slave` над картой регистров с адресом slave'а (вход
`domain::server::ServerWiring`).{% else %}
В этом проекте это задача в `crates-cross/app/src/tasks/modbus.rs`: она зовёт
`domain::server::serve` с каналом `crate::link::<вид>::<транспорт>::link` над
рычагом платы и обработчиком `domain::modbus::Slave` над
картой регистров с адресом slave'а.{% endif %} Карта одна на проект: её
заводит `crates-cross/app/src/modbus.rs` в `static` (`OnceLock`) при первом
`crate::modbus::handle()`, и каждый узел берёт свою копию ручки
(`crate::modbus::Handle` = `domain::modbus::SharedRegisters<Map>`, `Copy`) — запись через один транспорт видна чтению через
другой. Ручка берёт мьютекс карты (`domain::modbus::MapMutex`,
`CriticalSectionRawMutex` — `OnceLock` в `static` требует `Sync`) на время
одного вызова порта, так что запросы с разных транспортов идут по
очереди.{% if ota == "true" %} Окно OTA (`domain::modbus::OtaRegisters`) каждый узел оборачивает вокруг
своей копии ручки снаружи, вне блокировки: `Start` ждёт стирания секундами, а
карту в это время обслуживают остальные транспорты. Номер клиента OTA у окна —
порядковый номер транспорта (`docs/ota.md`).{% endif %}

Свою карту проект подставляет в одном месте — псевдоним `Map` в
`crates-cross/app/src/modbus.rs` (по умолчанию `domain::modbus::EmptyRegisters`):
тип реализует `ports::Registers` и живёт в `domain`. Карта строится через
`Map::default()`; без `Default` замените эту одну строку в `crate::modbus::handle`
своим конструктором. Узлы{% if graph == "true" %} и граф{% endif %} тип берут оттуда и правок не требуют.
{%- endif %}

# Питание от батареи: STOP между задачами

Исполнитель по умолчанию засыпает в `WFE` — ядро стоит, но регуляторы и
тактирование работают, и это единицы миллиампер. Для устройства на батарее
нужен режим STOP, где остаётся жить только то, что вы попросили: там речь
про единицы микроампер. `embassy-stm32` (фича `low-power`) уходит в STOP
функцией `low_power::sleep`: перед сном она заводит будильник RTC на
ближайший таймер и останавливает драйвер времени, после пробуждения
досчитывает проспанное. Шаблон этого за вас не делает: меняется сон
исполнителя{% if graph == "true" %} и цепочка таймаутов сторожа{% endif %}, а выигрыш получит не каждая плата —
USB, Ethernet и CAN в STOP не работают.

Что менять, если вы за это взялись (`embassy-stm32` 0.6):

```ignore
// 1. crates-cross/Cargo.toml, embassy-stm32: + "low-power". Драйвер времени
//    оставить: TIM-драйвер с этой фичей сам заводит будильник RTC перед STOP
//    и стоит, пока чип спит; `time-driver-lptim*` — альтернатива, не условие.
// 2. Там же, embassy-executor: убрать "platform-cortex-m". Её `__pender`
//    будит `WFE` через `SEV`, а `sleep()` спит в `WFI`: пробуждение задачи
//    из самого `poll()` (pender там зовётся синхронно) пролежало бы до
//    следующего прерывания. Взамен — свой pender с флагом (так устроен и
//    `embassy_stm32::executor`, но его флаг `pub(crate)`, взять нельзя):
//
//      static WORK: AtomicBool = AtomicBool::new(false);
//      #[unsafe(export_name = "__pender")]
//      fn pender(_: *mut ()) { WORK.store(true, Ordering::SeqCst); }
//
// 3. main.rs: цикл остаётся ручным, меняется только сон — вместо `wfe()`:
//
//      critical_section::with(|cs| {
//          if !WORK.swap(false, Ordering::SeqCst) {
//              // SAFETY: периферию, которой нужен сон без STOP, держат
//              // драйверы HAL; своей через PAC в проекте нет.
//              unsafe { embassy_stm32::low_power::sleep(cs) };
//          }
//      });
//
//    Замер сна для {% if graph == "true" %}узла `CPU_LOAD`{% else %}загрузки{% endif %} не ломается: после пробуждения драйвер
//    времени досчитывает проспанное по RTC, и `Instant::now()` вокруг сна
//    верен.
```

{% if graph == "true" %}Пять вещей{% else %}Четыре вещи{% endif %}, на которых спотыкаются:

* **Ближе 250 мс таймер не пускает в STOP.** Если до ближайшего таймера
  меньше `min_stop_pause` (`embassy_stm32::Config`, по умолчанию 250 мс),
  `sleep()` остаётся в обычном сне.{% if graph == "true" %} Тикер сторожа графа будит каждые 100 мс
  (`WATCHDOG_CHECK_EVERY` в `crates-cross/app/src/graph.rs`) — с ним чип в
  STOP не уйдёт никогда. Период тикера поднимают выше паузы, а вместе с ним
  и `HW_TIMEOUT_US` с таймаутами узлов — цепочка `docs/watchdog.md`.{% endif %}
  Периоды узлов шаблона (`heartbeat`, `cpu_load` — 500 мс) паузу проходят.
* **Занятая периферия не пускает в STOP.** `sleep()` смотрит на состояние
  RCC: пока жив объект `Uart`/`I2c`/`Spi`, чип останется в обычном сне.
  Держать их в статике «чтобы не пересоздавать» — ровно то, что съест
  экономию; создавайте на время обмена и роняйте.
* **Отладочный порт в STOP по умолчанию жив.** `enable_debug_during_sleep`
  у `embassy_stm32::Config` по умолчанию `true`: RTT и `probe-rs attach`
  работают, но и ток выше. Ради настоящих микроампер его выключают в
  `config()` (`crates-cross/bsp/src/board.rs`) — тогда лог в STOP молчит.
* **RTC занят будильником.** С `low-power` RTC забирает драйвер времени, и
  `Rtc::new` приложению не достанется. Кварц не обязателен: по умолчанию
  RTC тикает от LSI (на L5 источник выключен — задайте `config.rcc.ls`),
  но точность времени во сне — точность LSI.
{%- if graph == "true" %}
* **Сторож в STOP не спит.** IWDG тикает от LSI и продолжает считать.
  Опционный байт `IWDG_STOP` не нужен: тикер просыпается по RTC чаще
  таймаута — если период тикера по правилу выше короче `HW_TIMEOUT_US`.
  Остановить уже запущенный IWDG нельзя — это его свойство, а не недосмотр
  (см. `bsp::wdg`).
{%- endif %}

# PANIC и данные, переживающие сброс

Если у чипа хватило RAM, генерация отрезала от её конца регион `PANIC` — под
дамп `panic-persist` (им занят паникёр release-профиля, трогать не надо).
Адресуется он символами из `memory.x`, а не `link_section`-секцией, и это
ограничение, а не стиль: своя секция в конце RAM ломает `flip-link` (он
считает занятую часть RAM по адресам секций, видит секцию у верхней границы
и оставляет `_stack_start` в начале RAM — первый же push уходит за границу,
`HardFault`, `LOCKUP`).

Своих данных, переживающих сброс, шаблон не заводит. Понадобятся — берите
штатную `.uninit` от `cortex-m-rt`: её `flip-link` понимает,
`#[unsafe(link_section = ".uninit.MY")]` работает как обычно, а ценой будет
плавающий от сборки к сборке адрес (то есть данные не переживут смену
образа). Нужен фиксированный адрес — режьте регион при генерации, как это
сделано с `PANIC` (`chip-data-gen`, `chip-select.rhai`), и отдавайте его
наружу объектом за портом из `bsp`, а не голым указателем из `app`.
