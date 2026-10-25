{% if graph == "true" %}Оркестрация embassy-задач: жизненный цикл, рестарты, watchdog,
межзадачный обмен.{% else %}Оркестрация embassy-задач: загрузка процессора, логгер без пробника,
питание от батареи.{% endif %}

Примеры ниже намеренно не компилируются (`ignore`) — этот `cross`-workspace
не может собраться в сыром шаблоне: `embassy-stm32` завязан на конкретный
`{{chip_feature}}`, подставляемый только при генерации проекта
(`cargo generate`). Раскомментируйте и адаптируйте под свою плату по мере
необходимости.
{%- if graph == "true" %}

# `supervisor` — граф задач: порядок старта, рестарты, обмен, watchdog

Один макрос `supervisor_graph!` описывает весь набор задач сразу. Он
заменил три сторонних крейта, каждый из которых описывал свой срез того
же графа своим DSL: `embassy-supervisor` (порядок старта по зависимостям),
`embassy-task-watchdog` (мультиплексор watchdog'ов) и `ector` (актор с
почтовым ящиком). Раньше один и тот же граф приходилось объявлять трижды
и следить за тем, чтобы три декларации не разъехались.

Что даёт узел графа, помимо `deps:` (упорядоченный старт — через явный
барьер готовности, а не порядок спавна) и `restart:`/`backoff:`
(политика перезапуска с backoff'ом; узлы шаблона ею не пользуются — они не
падают, `restart: Never`: на отказ связи пишут в лог и продолжают, а
пережидает отказ реализация порта):

* `resources: [SLOT: Type]` — эксклюзивная передача владения (обычно
  ручкой периферии) в задачу на один прогон и обратно при её остановке,
  чтобы следующий перезапуск получил тот же объект. Отвечает на вопрос,
  который сам по себе `Spawner` не решает: как перезапустить задачу,
  забравшую `Peripherals`-ручку.
* `inbox: [FIELD: Type; N]` — ограниченная упорядоченная очередь входящих
  сообщений (это и есть бывший `ector`: fire-and-forget между задачами).
  Прямо стыкуется с `fsm_async::AsyncTimedRuntime::run` — см. ниже.
* `publish: [FIELD: Type; N]` / `subscribe: [OTHER.FIELD]` — broadcast
  состояния наружу через `embassy_sync::watch::Watch`.
* `flags: [FIELD: Type]` — слово статуса подсистемы (`flags::Word`, битовые
  флаги): владелец получает `flags::Writer`, читатели — `flags::Reader` через
  тот же `subscribe:`, без счётчика подписчиков.
* `request: [FIELD: Req -> Resp]` / `calls: [OTHER.FIELD]` — request/response
  поверх `sync_request::RpcService`, с `RequestTimeoutExt::request_timeout`
  для таймаута. `Req`/`Resp` обязаны укладываться в 32 байта — больше
  задаётся явно, `[FIELD: Req -> Resp; 128]`, или на весь узел фрагмента
  `request_capacity: 128;`. Отвечать — `served.respond_with(fut).await`, а не
  `respond(fut.await)`: вторая форма стоит лишнего `memmove` на флеше.
* `watchdog: Duration` — участие узла в общем аппаратном watchdog'е,
  см. следующий раздел.
* `shutdown: Cooperative` — узел не отменяют дропом посреди работы:
  задача получает `stop: impl Stoppable` и выходит сама, доделав то, что
  нельзя бросить на середине (запись во flash, транзакция на шине).
* `executor: NAME` — узел спавнится не на общем исполнителе, а на том,
  чей `SpawnerSlot` объявлен в графе (прерывательный приоритет, второе
  ядро).
* `cloned:` / `shared:` — общий на весь граф объект: первый отдаёт
  каждому узлу собственный `Clone`, второй — `&'static` на один
  физический ресурс (шина под `Mutex`).

Полный список полей и их тонкости — в doc-комментариях самого
`supervisor` и `supervisor-macros`; здесь только минимальный каркас.

**Узел графа — это фрагмент, и живёт он в `domain` вместе со своей
задачей.** Граф собственных узлов не объявляет вовсе: каждый приходит из
`supervisor_fragment!`, а `supervisor_graph!` здесь — это `boot:`-объект,
блок `watchdog:` и список `fragments:`. Держится это на двух возможностях
`supervisor`, и обе стоит знать, потому что растить проект вы будете именно
ими.

**Первая — контекст, который эмитит фрагмент.** Узел пишется с
`ctx: $crate::путь::ИмяCtx, task: $crate::путь::задача;`: `supervisor_fragment!`
эмитит структуру контекста (поля — слоты, каналы, `heartbeat`, `index`) в
крейте фрагмента, а не compose-site, поэтому сигнатура задачи не называет
ничего из крейта с графом. **Каждая задача — автомат `#[fsm::typestate]` на
этом контексте**: таблица состояний и переходов стоит на `impl ИмяCtx`, действия
— его методы, задача — `match ctx.run().await {}` (или `ctx.run().await.result`,
если у автомата есть выходное состояние). Нелегальный переход и необработанное
событие так не компилируются. Форма `task: имя` без `ctx:` тоже работает, но
тип контекста тогда генерирует compose-site, и задача привязана к `cross`;
проекция аргументов (`task: путь(ctx.поле)`) контекста не даёт вовсе — автомат
стоять не на чем.

**Вторая — фрагменты.** `supervisor_fragment!` объявляет самодостаточный
срез графа — один узел (без имени) с его политикой, таймаутом, слотами и
задачей, плюс, если надо, свои `cloned:`/`shared:` и собственная
`boot:`-привязка — в крейте, который сам никакого графа не объявляет. Узел
`HEARTBEAT` шаблона объявлен так — в `crates-host/domain/src/heartbeat.rs`:

```ignore
supervisor::supervisor_fragment! {
    name: HEARTBEAT_FRAG;

    node deps: [], restart: Never,
        watchdog: $crate::heartbeat::WATCHDOG observe,
        ctx: $crate::heartbeat::HeartbeatCtx,
        task: $crate::heartbeat::task;
}

pub async fn run(mut ctx: HeartbeatCtx<'_>) -> TaskExit {
    match ctx.run().await {}
}

#[fsm::typestate]
impl HeartbeatCtx<'_> {
    fsm::machine! {
        machine Heartbeat mod edges {
            states { Idle { next: Instant } }
            start => begin -> Idle;
            listen { Idle { next } => tick: fsm::deadline(*next) }
            transitions { Idle + tick(()) => idle -> self }
        }
    }
    // begin(), idle() — методы контекста; idle() переносит срок:
    // s.next = fsm::next_tick(s.next, IDLE_PERIOD, Instant::now())
    // (не `Ticker`: отставший догоняет пачкой тиков подряд).
}
```

а этот файл его подбирает и называет: `fragments: [::domain::HEARTBEAT_FRAG as HEARTBEAT]`
(см. ниже, в настоящем графе). Вторая подсистема добавляется так же — своим
фрагментом рядом со своим кодом и одной записью в `fragments:`. Тот же фрагмент
под двумя `as` — два независимых узла со своими статиками; `as NAME[N]` — N
копий под одним именем (`NAME_0`, `NAME_1`, …).

Три вещи, на которых тут спотыкаются:

* **Имя узлу даёт запись `fragments:`, а не фрагмент.** `as HEARTBEAT` — узел
  `HEARTBEAT`; без `as` узел зовётся как фрагмент (`HEARTBEAT_FRAG`). Всё, что фрагмент
  объявляет сверх узла (слоты, `provide_*`, `publish:`-приёмники), получает
  префикс этого имени: `resources: [DEVICE: …]` у `… as SENSOR` — это слот
  `SENSOR_DEVICE` и `provide_sensor_device`.
* **Токены фрагмента резолвятся на compose-site**, а не в крейте, где он
  написан: `macro_rules!` подставляет их в место вызова. Поэтому всё, что
  фрагмент называет, пишется полным путём — `$crate::heartbeat::WATCHDOG`,
  `$crate::heartbeat::task` — и от этого файла ничего не требует;
  голое имя искалось бы здесь и падало бы с `E0425`. `$crate::` при этом
  указывает в крейт фрагмента и переживает переименование зависимости в чужом
  манифесте, а `crate::` означал бы крейт, который фрагмент вызывает. Типы,
  которых фрагмент назвать не может (адаптер платы в слоте), — его параметры:
  {% if ota == "true" %}`name: OTA_FRAG<F, R>` во фрагменте, `::domain::OTA_FRAG<::adapters::ota::Updater<bsp::FirmwareFlash, true>, bsp::reset::Reboot>` здесь{% else %}`name: SENSOR_FRAG<R>` во фрагменте, `::domain::SENSOR_FRAG<SensorResources>` здесь
  (пример ниже){% endif %}.
* **Порядок items фиксирован**: `name:` → `boot:` → `watchdog:` →
  `cloned:`/`shared:` → `executor` → `fragments:` последним. `fragments:`
  раньше `boot:` — ошибка разбора; блок `watchdog:` внутри фрагмента — тоже
  (это решение на весь граф, фрагмент лишь опрашивает его полем `watchdog:`
  своего узла).

Цена всей схемы — зависимость `domain` от `supervisor` (ради `Heartbeat` и
`TaskExit` в сигнатуре задачи, `Duration` в таймауте); она
разобрана в `docs/architecture.md`. Сам фрагмент её не требует: он не
генерирует задач, а `supervisor` по имени обязан быть только у compose-site.

Время везде — `embassy_time::Duration`, единственный тип времени во всей
библиотеке (`supervisor::Duration` — он же, реэкспортом). `core::time::Duration`
из неё убран намеренно: его представление `{secs, nanos}` превращает каждую
конверсию в 64-битное деление, которое на 32-битной цели раскрывается в вызов
`__aeabi_uldivmod`, тогда как `embassy_time` — это просто счётчик тиков.

Ниже — куда его растить: пример показывает только то, чего в минимальном узле
нет — зависимости, ресурсы, почтовый ящик и тип, который знает лишь плата.

```ignore
// в crates-host/domain/src/sensor.rs: слот под ручку периферии, тип которой
// `domain` не видит, — отсюда параметр фрагмента.
supervisor::supervisor_fragment! {
    name: SENSOR_FRAG<R>;
    // Узлы шаблона не падают: на отказ автомат возвращается в начальное
    // состояние, перезапускать графу нечего.
    node deps: [], restart: Never,
        resources: [BUS: R],
        ctx: $crate::sensor::SensorCtx,
        task: $crate::sensor::run;
}

// в crates-host/domain/src/link.rs — то есть там, где это тестируется на хосте:
supervisor::supervisor_fragment! {
    name: LINK_FRAG;
    // Стартует только после того, как SENSOR сигнализировал готовность.
    node deps: [SENSOR], restart: Never,
        inbox: [EVENTS: $crate::link::LinkEvent; 8],
        ctx: $crate::link::LinkCtx,
        task: $crate::link::run;
}

pub async fn run(mut ctx: LinkCtx<'_>) -> TaskExit {
    match ctx.run().await {} // не возвращается; отменяется дропом при shutdown
}

#[fsm::typestate]
impl LinkCtx<'_> {
    fsm::machine! {
        machine Link mod edges {
            states { Disconnected, Connected { peer: u8 } }
            start -> Disconnected;
            // Почтовый ящик узла — обычный источник событий.
            listen { * => ev: self.events.receive() }
            transitions {
                Disconnected + ev(LinkEvent::Up(peer)) => up   -> Connected,
                Connected    + ev(LinkEvent::Down)     => down -> Disconnected,
                // Каждое событие каждого состояния — строкой: забытое не
                // скомпилируется (`E0004`), `_ =>` здесь нет.
                Disconnected + ev(LinkEvent::Down)     => ignore,
                Connected    + ev(LinkEvent::Up(_))    => ignore,
            }
        }
    }
    // up(), down() — методы контекста
}

// здесь, в graph.rs: две записи вместо одной.
fragments: [::domain::HEARTBEAT_FRAG as HEARTBEAT,
            ::domain::SENSOR_FRAG<SensorResources> as SENSOR,
            ::domain::LINK_FRAG as LINK];

// в main(), после инициализации HAL: слот назван по узлу и по слоту.
provide_sensor_bus(r.sensor).expect("слот пуст до первого spawn_all");
// Второй параметр — `boot:`-объект графа; в шаблоне это `board`
// (`bsp::Board`: рычаги платы). Слот,
// написанный с инициализатором — во фрагменте `resources: [BUS: R = hw.sensor]`
// при его собственной `boot: hw: …;`, а здесь `… as SENSOR = board.sensor`, —
// заполняет сам граф:
// строки `provide_sensor_bus` тогда нет вовсе, а значит её нельзя и забыть
// (так устроен узел {% if ota == "true" %}OTA{% else %}CPU_LOAD{% endif %} ниже). Формой не обслуживаются слоты
// `consume`/`shared` (их переиздаёт `respawn_all`, а boot-объект к тому
// моменту потреблён) и всё, что требует `await` или повтора, — для этого
// остаётся узел-инициализатор.
spawn_all(&spawner, board).expect("узлы свежие");
```

**Большой объект в слот по значению не кладут.** Слот `[X: T]` — `static`
с `Option<T>`, и значение дважды проходит через стек: его строит пролог
`spawn_all` (`= <expr>`, кадр `main`) или вызывающий `provide_*`, а задача забирает его `take()` в первом
опросе — копией на стек и оттуда в future в `TaskStorage`. Живёт объект тоже
дважды: в слоте и в future. Стек под `flip-link` — это остаток RAM после
статиков, своего размера у него нет, поэтому то, что «влезло» в одном проекте,
в другом с парой лишних буферов уходит ниже начала RAM (`HardFault`, SP вне
памяти). Всё, что от килобайта и выше, — слот `&'static mut T` со статиком,
который заводит сам граф:

```ignore
resources: [WS: &'static mut $crate::x::Workspace = static],
// или с «заплаткой» для полей, которые не должны быть нулём:
resources: [WS: &'static mut $crate::x::Workspace = static |w| w.gain = inputs.gain],
```

Статик — `cloned_cell::ZeroedCell<T>` (его заводит граф внутри `spawn_all`,
своей зависимости не нужно), нули, то есть `.bss` без образа во флеше; `T` на
стеке не строится ни разу. Цена: `T: supervisor::Zeroed`
(`unsafe impl supervisor::Zeroed for Workspace {}` под `// SAFETY:` с полями;
целые, `bool`, `()`, массивы из них и `Option<&T>` уже реализуют), слот без
пометок — не `consume`, не `shared`, не `local`. Ссылка выдаётся один раз:
второй `spawn_all` вернёт `GraphSpawnErrorKind::BaseClaimed`, а
`respawn_all` объект не обнуляет — узел получает его с тем, что записал.

Слот по значению стоит ещё и флеша: его образ — `None` у `Option<T>`, а при
нише в `T` (один `bool` или enum без полей внутри) это не нули, и статик
целиком уходит в `.data` — размером `T` во флеше сверх RAM. Свой `StaticCell`
в `app` вместо `= static` не нужен: `StaticCell::init_with(|| T::new(..))` не
обещает строить на месте, а `ConstStaticCell` с ненулевым `T` уводит статик в
ту же `.data`. Подробности — `supervisor-macros/docs/dsl.md`, «A large object:
`= static`» и «What those statics cost in flash».

# `watchdog` — мультиплексор задач в один аппаратный watchdog

`supervisor` пользуется им внутри (блок `watchdog:` выше), а реализовать
под свой МК нужно ровно один трейт — `HardwareWatchdog`. Часов крейт не
держит вовсе: `now` он принимает параметром, и подаёт его туда сам граф
(`embassy_time::Instant::now()`). Раньше здесь был второй трейт, `Clock`,
и его убрали не ради краткости: параметр не требует ни impl'а, ни
типажа, принимает любую шкалу времени (например всегда живой RTC/LPTIM —
драйвер `embassy_time` в STOP/STANDBY встаёт, и watchdog слеп ровно
тогда, когда МК может не проснуться), а прежний `core::time::Duration`
стоил вызова `__aeabi_uldivmod` на каждой конверсии.

Реализация привязана к железу, поэтому её место — `bsp`, и она там уже
есть: `bsp::wdg::Iwdg` (см. `crates-cross/bsp/src/wdg.rs`).
Писать её руками не нужно, и подключать тоже: имя блока подставлено при
генерации из метаданных чипа (`IWDG`, на H7 `IWDG1`, на двухъядерных H7 у
ядра CM4 `IWDG2`), объект собирает `Board::new`, а запускает его блок
`watchdog:` графа — инициализатором `= board.watchdog.arm()`, где
`board` это объявленный выше `boot:`-объект, который `main` передаёт в
`spawn_all`. Всё, что осталось на вас, — цифры, и живут они там, о чём
каждая: `WATCHDOG_CHECK_EVERY` здесь (про один сторож на всех), таймаут узла
во фрагменте (про этот узел, вместе с периодом его отметок — и их соотношение
держит `const`-assert там же; узлов в шаблоне два, `HEARTBEAT` и `CPU_LOAD`, и
цифры у них свои), `bsp::wdg::HW_TIMEOUT_US` у железа. Второе соотношение
(таймаут узла плюс `WATCHDOG_CHECK_EVERY` меньше `HW_TIMEOUT_US`) держит
`const`-assert рядом с `WATCHDOG_CHECK_EVERY`: второго места, где видны оба
конца, нет, а `supervisor` намеренно их не связывает.

Узел объявлен **наблюдаемым** (`watchdog: ... observe`): его просрочка
докладывается наблюдателю графа (`report_liveness`) и не превращается в
аппаратный сброс. Так шаблон не раздаёт проект, который перезагружает сам
себя, стоит добавить в узел работу длиннее его таймаута. Уберёте слово
`observe` — у узла появится настоящая аппаратная подстраховка, и тогда же
стоит перечитать оба соотношения таймаутов в `bsp::wdg::Iwdg::arm`.
Наблюдаемость узла ничего не меняет для железа: сторож взведён и кормится
тикером графа, поэтому вставший целиком исполнитель сбрасывает плату в
любом случае.

Важное следствие: `Heartbeat::feed()` узел зовёт **явно**, из тела своей
задачи, и только когда реально продвинулся. Автоматического «задача жива,
раз её future опрашивают» здесь нет — иначе watchdog сторожил бы
исполнитель, а не полезную работу.

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
ровно как у узла `OTA` ниже{% endif %}.
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

Понадобится автомат состояний (например, крейты `fsm`/`fsm-async` из
[rust-lib]({{rust_lib}})) — его место в `domain`, а
не здесь, даже если он владеет своей задачей целиком и ждёт события из
`embassy_sync`-канала: сама модель состояний остаётся чистой логикой без
привязки к железу, а `cross` лишь спавнит задачу, гоняющую `run().await`{% if graph == "true" %},
и кормит её событиями через `inbox:` (см. `domain::link::run` выше){% endif %}.

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
{%- if ota == "true" and graph == "true" %}

# OTA: узел уже в графе, вам остаётся канал

Узел `OTA` объявлен в `domain::ota` (`OTA_FRAG`; режим применения —
`Mode::Signed` — подставила генерация во вход) и спавнится отсюда записью в
`fragments:` — она же даёт ему имя (`as OTA`) и типы двух его слотов
аргументами. Образ он принимает командами (`ports::OtaCommand`) на свой вход
`CMD` — `request:` фрагмента, запрос с ответом (или на `ASK` — `serves:`, для
клиента, который не ждёт исхода в вызове): `Start` (сверка длины до
стирания), `Chunk` (буферизация до слова флеша — `domain::ota::download::Transfer`),
`Finish` (`mark_updated` или `domain::ota::update::apply_signed`), `Reboot`, — и на
каждую отвечает исходом `ports::OtaReply`. Всё это — host-тесты в
`crates-host/domain/src/ota.rs`.

Команды шлёт узел протокола: клиента входа отдаёт аксессор графа
`ota_cmd_requester()`, а узел получает его обёрнутым в `domain::ota::OtaClient`
с номером своего транспорта (`ClientId`, порядковый номер транспорта с
протоколом в `links`, с 0){% if has_protobuf == "true" %} — его получает `domain::host::Service`
прямо во входе узла `HOST_*`{% endif %}. Номер нужен
владельцу сессии: чужой приём, пока идёт свой, получает `Busy`
(`docs/ota.md`). Своего канала у образа нет: он идёт по протоколу,
которым хост и так говорит с устройством (`docs/ota.md`).

Плата отдаёт для узла флеш под обновлением `board.firmware`
(`bsp::FirmwareFlash` — `adapters::ota::Linked`, ещё не нарезанный) и порт
перезагрузки `bsp::reset::Reboot`; эта сторона кладёт их во вход фрагмента, а
адаптер обновления собирает сам узел (`ports::Assemble`), нарезая флеш на `DFU` и
`STATE` по символам `memory.x`: `bsp` про `domain` не знает
(`= ::domain::ota::OtaWiring { source: board.firmware, reboot:
bsp::reset::Reboot, mode: ::domain::ota::Mode::Signed { running_version:
crate::version::FW_VERSION } }`). Типы слотов — аргументы фрагмента
(`OTA_FRAG<::adapters::ota::Updater<bsp::FirmwareFlash, true>, bsp::reset::Reboot>`):
слот — `static`, тип ему нужен, а знает его только эта сторона. Слот адаптера — `local` (фича
`supervisor/local-resources` в
`crates-cross/Cargo.toml`): объекты платы `!Send`, и граф держит их на своём
исполнителе, не требуя `Send`. Узел, чей `local`-слот заполняется
инициализатором (здесь — из `inputs`), нельзя перевести на другой
исполнитель (`executor:`); захотите вынести OTA туда — менять `local` во
фрагменте `domain::ota` и `NoopRawMutex` у {% if ota_store == "external" %}`bsp::external::ExternalMutex`{% else %}`bsp::flash::FlashMutex`{% endif %} вместе.

У узла нет `watchdog:`, и это не забывчивость: команды может не быть всю
жизнь платы, и прогресса, о котором можно отметиться, у узла нет. Аппаратный
сторож кормит тикер графа, а не какой-то узел.
{%- endif %}
{%- if has_modbus == "true" %}

# Modbus: одна карта на все узлы `MODBUS_*`

Узел `MODBUS_<ТРАНСПОРТ>` — по одному на транспорт с Modbus в `links`.{% if graph == "true" %}
Фрагмент — узел протокола `domain::SERVER_FRAG<L, H>`: канал
`L` (`crate::link::<вид>::<транспорт>::Link`) узел собирает сам из рычага платы и
буферов `crate::link::<вид>::<транспорт>::buffers()`, обработчик `H` —
`domain::modbus::Slave` над картой регистров с адресом slave'а (вход
`domain::server::ServerWiring`).{% else %}
В этом проекте это задача в `crates-cross/app/src/tasks/modbus.rs`: она зовёт
`domain::server::serve` с каналом `crate::link::<вид>::<транспорт>::Link`, который
узел собирает из рычага платы, и обработчиком `domain::modbus::Slave` над
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
