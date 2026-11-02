Крейт `app`: загрузка процессора и ручной цикл исполнителя, логгер без
пробника, питание от батареи, данные, переживающие сброс.

Примеры ниже намеренно не компилируются (`ignore`): им нужна периферия
конкретной платы, которой шаблон не знает. Раскомментируйте и адаптируйте под свою плату по мере
необходимости.
{%- if graph == "true" %}

# `CPU_LOAD`: загрузка процессора, и почему цикл исполнителя ручной

Второй узел шаблона, кроме `HEARTBEAT`, — `CPU_LOAD`: он меряет, во сколько
процентов занят процессор, и публикует результат в `PERCENT`. Фрагмент —
`crates-host/domain/src/cpu_load.rs`, арифметика и её host-тесты —
`crates-host/domain/src/cpu_load/load.rs`, счётчик и объяснения —
`app-cpu-load.md`.

Читать загрузку — ваше дело: как, разово или подпиской, — `app-cpu-load.md`,
«Кто читает загрузку».
Обратите внимание, чего нет в задаче `start` перед `spawn_all`: вызова
`provide_cpu_load_sleep_ticks(..)`. У слота `SLEEP_TICKS` узла `CPU_LOAD` есть
инициализатор (`= inputs.sleep_ticks`) из собственной `boot:`-привязки
фрагмента (`IdleMeter` в записи `fragments:`), поэтому заполняет его сам граф,
и забыть строку нельзя{% if ota == "true" %} —
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

`panic.rs` держит `defmt-rtt` дефолтным транспортом в обоих профилях —
шаблон не знает, какой конкретный USART{% if has_usb == "true" %}/USB{% endif %} плата отведёт под лог
без пробника (RTT его не даёт, отладочный пробник должен быть физически
подключён). Ниже — паттерн на замену для `release`, когда плата начнёт
отдавать рычаг UART под лог. **При его подключении обязательно сделайте
`use defmt_rtt as _;` в `panic.rs` `#[cfg(debug_assertions)]`** (сейчас он
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
против актуального `defmt@1.x`. Grant/commit API `bbqueue` (0.7) уже
объяснён в `docs/buffers.md` (там — DMA-буфер, тут — очередь под
лог); ниже — только то, что специфично именно для `#[global_logger]`:

```ignore
use bbqueue::{
    queue::BBQueue,
    traits::{coordination::{% if core_baseline == "true" %}cs::CsCoord{% else %}cas::AtomicCoord{% endif %} as Coord, notifier::polling::Polling, storage::Inline},
};
use defmt::{Encoder, Logger};

{% if core_baseline == "true" %}// Cortex-M0/M0+ без CAS: координация на критической секции — фича
// `critical-section` у `bbqueue`.
{% endif %}static QUEUE: BBQueue<Inline<1024>, Coord, Polling> = BBQueue::new();
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
        // QUEUE.stream_producer().grant_max_remaining(bytes.len()),
        // copy_from_slice, commit
    }
}

// отдельная задача, только под release — DMA-запись consumer-половины
// очереди в USART, аналогично примеру из docs/buffers.md. Тип UART — псевдоним
// рычага в `bsp` (`app` про `embassy-stm32` не знает, а задача embassy не
// generic), например `pub type LogTx = UartTx<'static, mode::Async>;`:
#[embassy_executor::task]
async fn uart_drain_task(mut uart: bsp::LogTx) {
    let consumer = QUEUE.stream_consumer();
    loop {
        match consumer.read() {
            Ok(grant) => {
                let len = grant.len();
                let _ = uart.write(&grant).await;
                grant.release(len);
            }
            // Очередь пуста: отдать исполнитель, а не крутиться в цикле.
            Err(_) => embassy_time::Timer::after_millis(10).await,
        }
    }
}
```
{%- if protocol_link == "true" %}
{%- capture sources %}{% if has_uart == "true" %}, потоком UART{% endif %}{% if has_usb == "true" %}, потоком CDC{% endif %}{% if has_net == "true" %}, сетевым стеком{% endif %}{% endcapture %}

# Канал протокола — конкретным типом там, где строится

Канал протокола (кадрирование{% if has_net == "true" %}, сокет{% endif %}) — тип из `adapters-logic` над тем, что
отдала плата: {{ sources | remove_first: ", " }}. Строит его {% if graph == "true" %}привязка узла в `fragments:`
графа (`crates-cross/app/src/graph.rs`): тип канала стоит параметром фрагмента,
конструктор — во входе узла, память — `static T` там же{% else %}задача протокола
(`crates-cross/app/src/tasks/<протокол>.rs`): рычаг платы — псевдоним `Source`
(задачи embassy не могут быть generic), канал — конкретным типом в теле
задачи, память — статики модуля задачи{% endif %}. Отдельного модуля «каналов» в
`app` нет: у каждого канала одно место сборки, и псевдоним над ним ничего не
добавлял бы. Размер буфера и порт — решение приложения, и `bsp` их не знает:
своё кадрирование — другой тип в том же месте, свой размер буфера или порт —
число там же, с комментарием, что оно значит.{% if has_udp == "true" %} Память метаданных датаграмм UDP
приходит в `adapters_logic::net::UdpLink::new` нулями (`MaybeUninit`), и
заполняет её сам конструктор.{% endif %}

Буфер кадра держит сам канал (`ports::FrameLink::buffer`), а протокол —
обработчик ({% if has_modbus == "true" %}`domain::modbus::Slave`{% if has_protobuf == "true" %}; {% endif %}{% endif %}{% if has_protobuf == "true" %}`domain::protobuf::Service`{% endif %}) — конкретный тип домена, трейта
под него нет: реализация одна.
{%- if has_modbus == "true" %} Узел Modbus slave — `::domain::SERVER_FRAG<L, R>`,
объявлен в `domain::server` ({% if graph == "true" %}граф называет узлы{% else %}узлы{% endif %} `MODBUS_<транспорт>`).{% endif %}
{%- if has_protobuf == "true" %} Узел protobuf хоста — `::domain::PROTOBUF_FRAG<L>`,
объявлен в `domain::protobuf` (`PROTOBUF_<транспорт>`); его канал —
`ports::FrameDuplex` (контракт — doc трейта), а ответ уходит из своего буфера
узла ({% if graph == "true" %}`static [u8; RESPONSE_MAX]` в привязке{% else %}статик модуля задачи{% endif %}) через `send_from`.{% endif %} Вход узла — {% if graph == "true" %}выражение
в `fragments:` графа{% else %}`spawn` модуля задачи (`crates-cross/app/src/tasks/`), по модулю на
задачу; `main` бинарника только зовёт `tasks::spawn_all`{% endif %}.
{%- endif %}
{%- if has_modbus == "true" %}

# Modbus: одна карта на все узлы `MODBUS_*`

Узел `MODBUS_<ТРАНСПОРТ>` — по одному на транспорт с Modbus в `links`.{% if graph == "true" %}
Фрагмент — узел протокола `domain::SERVER_FRAG<L, R>`: канал
`L` (раздел выше) строит граф — конкретным типом из `adapters_logic::modbus`
над рычагом платы в памяти `static T`, обработчик —
`domain::modbus::Slave` над картой регистров `R` с адресом slave'а (вход
`domain::server::ServerWiring`).{% else %}
В этом проекте это задача в `crates-cross/app/src/tasks/modbus.rs`: она собирает
контекст узла `domain::server::ServerCtx` — канал `adapters_logic::modbus::Rtu`
над рычагом платы и обработчик `domain::modbus::Slave` над
картой регистров с адресом slave'а — и зовёт `domain::server::task`.{% endif %} Карта одна на проект: её
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
тип реализует `domain::modbus::Registers` и живёт в `domain`. Карта строится через
`Map::default()`; без `Default` замените эту одну строку в `crate::modbus::handle`
своим конструктором. Узлы{% if graph == "true" %} и граф{% endif %} тип берут оттуда и правок не требуют.
{%- endif %}
{%- if ota == "true" %}

# Тестовый образ `app-unconfirmed`: свой бинарник, свои свойства образа

У пакета два бинарника, у каждого свой запуск: рабочий `app`
(`src/main.rs`) и тестовый `app-unconfirmed` (`src/bin/app-unconfirmed.rs`),
которым `cargo xtask test host-target` проверяет обмен и откат по протоколу
проекта. Всё, что не запуск, — библиотека пакета (`src/lib.rs`): узлы, паникёр,
логгер и обработчик `HardFault` из неё попадают в каждый бинарник по разу.
Запуск — ловушка деления, баннер, причины сброса и падения, ручной цикл
исполнителя с замером сна и задача `start` — у каждого бинарника свой, и
тестовый повторяет рабочий шаг в шаг, добавляя только `warn!` в логе старта.

Чем образы различаются, бинарник говорит сам — константой свойств образа
`IMAGE` (`app::version::Image`), которую отдаёт в `app::spawn_all`{% if graph == "true" %}
полем `image` `boot:`-объекта графа `app::Boot`{% endif %}:

* `version` — версия для защиты от отката: у `app` — `version` из Cargo.toml
  (`app::version::PACKAGE_VERSION`), у тестового — на единицу больше, ровно
  настолько, чтобы рабочий образ той же сборки принял его как обновление;
* `text` — версия текстом хосту и в баннере: у тестового — метка
  `unconfirmed`;
* `confirmed` — у тестового `true`: узел OTA `mark_booted` не зовёт, и
  следующий сброс откатывает образ.

Символ `FW_VERSION_IN_IMAGE` (версия, которую `cargo xtask` дописывает в хвост
образа) у каждого бинарника тоже свой. Фич Cargo для этого нет: голый `cargo
build` собирает оба бинарника, `cargo xtask build` и прошивка — только `--bin
app`, `cargo xtask test host-target` — `--bin app-unconfirmed`, `cargo xtask
lint cross` линтует оба; `cargo run -p app` берёт `app` (`default-run`).
Подробности — `docs/ota.md`, «Тесты OTA по протоколу».
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
// 3. bsp — про чип знает только он (`app` `embassy-stm32` не видит): модуль
//    `bsp::power` с одной функцией; `critical-section` — прямой зависимостью
//    `bsp` (реализацию даёт `cortex-m`, крейт в дереве уже есть):
//
//      pub fn sleep(work: &AtomicBool) {
//          critical_section::with(|cs| {
//              if !work.swap(false, Ordering::SeqCst) {
//                  // SAFETY: периферию, которой нужен сон без STOP, держат
//                  // драйверы HAL; своей через PAC в проекте нет.
//                  unsafe { embassy_stm32::low_power::sleep(cs) };
//              }
//          });
//      }
//
// 4. main.rs{% if ota == "true" %} (и bin/app-unconfirmed.rs){% endif %}: цикл остаётся ручным, меняется только сон —
//    вместо `wfe()` `bsp::power::sleep(&WORK)`.
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
  {% if graph == "true" %}Периоды узлов шаблона (`heartbeat`, `cpu_load` — 500 мс) паузу проходят{% else %}Период замера `cpu_load` (500 мс) паузу проходит{% endif %}.
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
  таймаута — если период тикера по правилу выше короче
  `bsp::wdg::HW_TIMEOUT_MIN_US` (таймаут на самом быстром LSI).
  Остановить уже запущенный IWDG нельзя — это его свойство, а не недосмотр
  (см. `bsp::wdg`).
{%- endif %}

# PANIC и данные, переживающие сброс

Регион `PANIC` в конце RAM занят дампом паникёра release-профиля — трогать
не надо; почему он адресуется символами, а не секцией, — `docs/diagnostics.md`,
«Регион `PANIC`».

Своих данных, переживающих сброс, шаблон не заводит. Понадобятся — берите
штатную `.uninit` от `cortex-m-rt`: её `flip-link` понимает,
`#[unsafe(link_section = ".uninit.MY")]` работает как обычно, а ценой будет
плавающий от сборки к сборке адрес (то есть данные не переживут смену
образа). Нужен фиксированный адрес — отрежьте свой регион в `memory.x`, как
отрезан `PANIC` (`crates-cross/app/memory.x`), и отдавайте его наружу
рычагом платы из `bsp` — конкретным типом, порт только под несколько
реализаций (`docs/architecture.md`, «Трейт — только под несколько
реализаций»), — а не голым указателем из `app`.
