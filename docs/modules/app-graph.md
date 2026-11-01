Оркестрация embassy-задач: жизненный цикл, watchdog, межзадачный
обмен. Остальное про крейт `app` — `app.md`.

Примеры ниже намеренно не компилируются (`ignore`) — этот `cross`-workspace
не может собраться в сыром шаблоне: `embassy-stm32` завязан на конкретный
`{{chip_feature}}`, подставляемый только при генерации проекта
(`cargo generate`). Раскомментируйте и адаптируйте под свою плату по мере
необходимости.

# `supervisor` — граф задач: порядок старта, обмен, watchdog

Один макрос `supervisor_graph!` описывает весь набор задач сразу — вместо
трёх сторонних крейтов, каждый из которых описывает свой срез того же
графа своим DSL: `embassy-supervisor` (порядок старта по зависимостям),
`embassy-task-watchdog` (мультиплексор watchdog'ов) и `ector` (актор с
почтовым ящиком). С ними один и тот же граф пришлось бы объявлять трижды
и следить за тем, чтобы три декларации не разъехались.

Что даёт узел графа в этом проекте{% if ota == "true" %}, помимо `deps:` (упорядоченный старт —
через явный барьер готовности, а не порядок спавна){% endif %}:

* `resources: [SLOT: Type]` — эксклюзивная передача владения в задачу на
  один прогон и обратно при её остановке. Узлы шаблона так получают рычаги
  платы и адаптеры над ними — из входа фрагмента (`= inputs.поле`).
* `inbox: [FIELD: Type; N]` — ограниченная упорядоченная очередь входящих
  сообщений (это и есть бывший `ector`: fire-and-forget между задачами).
  В автомате узла (`#[fsm::typestate]`) это обычный источник `listen` —
  см. пример ниже.
* `publish: [FIELD: Type; N]` / `subscribe: [OTHER.FIELD]` — broadcast
  состояния наружу через `embassy_sync::watch::Watch`.
{%- if ota == "true" %}
* `request: [FIELD: Req -> Resp]` / `calls: [OTHER.FIELD]` — request/response
  поверх `sync_request::RpcService`, с `RequestTimeoutExt::request_timeout`
  для таймаута. `Req`/`Resp` обязаны укладываться в 32 байта — больше
  задаётся явно, `[FIELD: Req -> Resp; 128]`, или на весь узел фрагмента
  `request_capacity: 128;`. Отвечать — `served.respond_with(fut).await`, а не
  `respond(fut.await)`: вторая форма стоит лишнего `memmove` на флеше.
{%- endif %}
* `watchdog: Duration` — участие узла в общем аппаратном watchdog'е,
  см. следующий раздел.
* `cloned:` / `shared:` — общий на весь граф объект: первый отдаёт
  каждому узлу собственный `Clone`, второй — `&'static` на один
  физический ресурс (шина под `Mutex`).

Остальные слова DSL — `restart:`/`backoff:` (узлам шаблона не нужны: они не
падают, `docs/architecture.md`, «Узлы не падают»), `shutdown:`, `flags:`,
`executor:`{% if ota != "true" %}, `deps:`, `request:`/`calls:`{% endif %} и др. — под фичами `supervisor`,
выключенными по умолчанию: слово без фичи — ошибка компиляции с её именем.
Включаются в манифесте: слова фрагментов `domain` — в
`crates-host/domain/Cargo.toml`, слова графа — в `crates-cross/Cargo.toml`
(`docs/first-node.md`). Полный список полей, фич и их тонкости — в
doc-комментариях самого `supervisor` и `supervisor-macros`; здесь только
минимальный каркас.

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
этом контексте** (`docs/architecture.md`, «Каждая задача узла — автомат
`#[fsm::typestate]`»). Форма `task: имя` без `ctx:` тоже работает, но
тип контекста тогда генерирует compose-site, и задача привязана к `cross`.

**Вторая — фрагменты.** `supervisor_fragment!` объявляет самодостаточный
срез графа — один узел (без имени) с его политикой, таймаутом, слотами и
задачей, плюс, если надо, свои `cloned:`/`shared:` и собственная
`boot:`-привязка — в крейте, который сам никакого графа не объявляет. Узел
`HEARTBEAT` шаблона объявлен так — в `crates-host/domain/src/heartbeat.rs`:

```ignore
supervisor::supervisor_fragment! {
    name: HEARTBEAT_FRAG;

    node watchdog: $crate::heartbeat::WATCHDOG observe,
        ctx: $crate::heartbeat::HeartbeatCtx,
        task: $crate::heartbeat::task;
}

pub async fn task(mut ctx: HeartbeatCtx<'_>) {
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
* **Токены фрагмента резолвятся на compose-site**, поэтому всё, что фрагмент
  называет, пишется полным путём (`$crate::heartbeat::WATCHDOG`; почему —
  `docs/architecture.md`, «Узлы — фрагменты в `domain`»). Типы,
  которых фрагмент назвать не может (адаптер над рычагом в слоте), — его параметры:
  {% if ota == "true" %}`name: OTA_FRAG<F, R, const PARKED: usize>` во фрагменте, `::domain::OTA_FRAG<::adapters::ota::Updater<bsp::FirmwareFlash>, bsp::reset::Reboot, OTA_CMD_PARKED>` здесь{% else %}`name: SENSOR_FRAG<R>` во фрагменте, `::domain::SENSOR_FRAG<bsp::SensorBus>` здесь
  (пример ниже){% endif %}.
* **Порядок items фиксирован**: `name:` → `boot:` → `watchdog:` →
  `cloned:`/`shared:` → `executor` → `fragments:` последним. `fragments:`
  раньше `boot:` — ошибка разбора; блок `watchdog:` внутри фрагмента — тоже
  (это решение на весь граф, фрагмент лишь опрашивает его полем `watchdog:`
  своего узла).

Цена всей схемы — зависимость `domain` от `supervisor` (`docs/architecture.md`,
«Цена решения…»). Сам фрагмент её не требует: он не
генерирует задач, а `supervisor` по имени обязан быть только у compose-site.

Время везде — `embassy_time::Duration`, единственный тип времени во всей
библиотеке (`supervisor::Duration` — он же, реэкспортом). `core::time::Duration`
из неё убран намеренно: его представление `{secs, nanos}` превращает каждую
конверсию в 64-битное деление, которое на 32-битной цели раскрывается в вызов
`__aeabi_uldivmod`, тогда как `embassy_time` — это просто счётчик тиков.

Ниже — куда его растить: пример показывает только то, чего в минимальном узле
нет — зависимости, ресурсы, почтовый ящик и тип, который знает лишь плата.

```ignore
// в crates-host/domain/src/sensor.rs: слот под рычаг платы, тип которого
// `domain` не видит, — отсюда параметр фрагмента. Вход `SensorWiring` эмитит
// сам фрагмент: по полю на каждое `inputs.<поле>`.
supervisor::supervisor_fragment! {
    name: SENSOR_FRAG<R>;
    boot: inputs: struct $crate::sensor::SensorWiring;
    // Узлы шаблона не падают: на отказ автомат возвращается в начальное
    // состояние, перезапускать графу нечего.
    node resources: [BUS: R = inputs.bus],
        ctx: $crate::sensor::SensorCtx,
        task: $crate::sensor::task;
}

// в crates-host/domain/src/peer.rs — то есть там, где это тестируется на хосте:
supervisor::supervisor_fragment! {
    name: PEER_FRAG;
    // Стартует только после того, как SENSOR сигнализировал готовность.{% if ota != "true" %}
    // `deps:` — фича `startup`, в этом проекте она выключена (`docs/first-node.md`).{% endif %}
    node deps: [SENSOR],
        inbox: [EVENTS: $crate::peer::PeerEvent; 8],
        ctx: $crate::peer::PeerCtx,
        task: $crate::peer::task;
}

pub async fn task(mut ctx: PeerCtx<'_>) {
    match ctx.run().await {} // не возвращается
}

#[fsm::typestate]
impl PeerCtx<'_> {
    fsm::machine! {
        machine Peer mod edges {
            states { Disconnected, Connected { id: u8 } }
            start -> Disconnected;
            // Почтовый ящик узла — обычный источник событий.
            listen { * => ev: self.events.receive() }
            transitions {
                Disconnected + ev(PeerEvent::Up(id))   => up   -> Connected,
                Connected    + ev(PeerEvent::Down)     => down -> Disconnected,
                // Каждое событие каждого состояния — строкой: забытое не
                // скомпилируется (`E0004`), `_ =>` здесь нет.
                Disconnected + ev(PeerEvent::Down)     => ignore,
                Connected    + ev(PeerEvent::Up(_))    => ignore,
            }
        }
    }
    // up(), down() — методы контекста
}

// здесь, в graph.rs: две записи вместо одной. Рычаг платы узел получает
// привязкой `= …` над полем `board` — `boot:`-объектом графа (`bsp::Board`).
fragments: [::domain::HEARTBEAT_FRAG as HEARTBEAT,
            ::domain::SENSOR_FRAG<bsp::SensorBus> as SENSOR =
                ::domain::sensor::SensorWiring { bus: board.sensor },
            ::domain::PEER_FRAG as PEER];

// в main(): `board` уходит в `spawn_all` целиком. Привязку вычисляет пролог
// `spawn_all` и кладёт рычаг в слот `SENSOR_BUS` сам — строки `provide_sensor_bus`
// нет вовсе, а значит её нельзя и забыть (так устроен узел
// {% if ota == "true" %}OTA ниже{% else %}`CPU_LOAD`, `app.md`{% endif %}). Формой не обслуживаются слоты
// `consume`/`shared` (их переиздаёт `respawn_all`, а boot-объект к тому
// моменту потреблён) и всё, что требует `await` или повтора, — для этого
// остаётся узел-инициализатор. `Debug` у ошибки графа нет, поэтому не
// `expect`, а `Display`:
if let Err(err) = spawn_all(&spawner, board) {
    defmt::panic!("app: spawn_all: {}", defmt::Display2Format(&err));
}
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

# Первый свой узел

Путь по шагам — модуль в `domain`, фича слова DSL, запись в `fragments:`,
сторож, рычаг платы и тест на хосте — `docs/first-node.md`.

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
`spawn_all`. Всё, что осталось на вас, — цифры: `WATCHDOG_CHECK_EVERY` здесь,
таймаут узла во фрагменте, `bsp::wdg::HW_TIMEOUT_US` у железа; их цепочка и
где какое звено проверяется — `docs/watchdog.md`.

Узел объявлен **наблюдаемым** (`watchdog: ... observe`): его просрочка
докладывается наблюдателю графа (`report_liveness`), а не сбрасывает плату
(`docs/watchdog.md`).

Важное следствие: `Heartbeat::feed()` узел зовёт **явно**, из тела своей
задачи, и только когда реально продвинулся. Автоматического «задача жива,
раз её future опрашивают» здесь нет — иначе watchdog сторожил бы
исполнитель, а не полезную работу.
{%- if ota == "true" %}

# OTA: узел уже в графе, вам остаётся канал

Узел `OTA` объявлен в `domain::ota` (`OTA_FRAG`; версию этого образа для
защиты от отката подставила генерация во вход) и спавнится отсюда записью в
`fragments:` — она же даёт ему имя (`as OTA`) и типы двух его слотов
аргументами. Образ он принимает командами (`ports::OtaCommand`) на свой вход
`CMD` — `request:` фрагмента, запрос с ответом (или на `ASK` — `serves:`, для
клиента, который не ждёт исхода в вызове): `Start` (сверка длины до
стирания), `Chunk` (буферизация до слова флеша — `domain::ota::download::Transfer`),
`Finish` (`domain::ota::update::apply`), `Reboot`, — и на
каждую отвечает исходом `ports::OtaReply`. Всё это — host-тесты в
`crates-host/domain/src/ota.rs`.

Команды шлёт узел протокола: клиента входа отдаёт аксессор графа
`ota_cmd_requester()`, а узел получает его обёрнутым в `domain::ota::OtaClient`
с номером своего транспорта (`ClientId`, порядковый номер транспорта с
протоколом в `links`, с 0){% if has_protobuf == "true" %} — его получает `domain::protobuf::Service`
прямо во входе узла `PROTOBUF_*`{% endif %}. Номер нужен
владельцу сессии: чужой приём, пока идёт свой, получает `Busy`
(`docs/ota.md`). Своего канала у образа нет: он идёт по протоколу,
которым хост и так говорит с устройством (`docs/ota.md`).

Плата отдаёт для узла разделы обновления `board.firmware`
(`bsp::FirmwareFlash` — `adapters::ota::Partitions`, нарезанные по символам
`memory.x`) и порт перезагрузки `bsp::reset::Reboot`; эта сторона кладёт их во
вход фрагмента вместе с адаптером обновления над разделами: `bsp` про `domain`
не знает
(`= ::domain::ota::OtaWiring { flash: Updater::new(board.firmware), reboot:
bsp::reset::Reboot, running_version: crate::version::FW_VERSION }`). Типы слотов — аргументы фрагмента
(`OTA_FRAG<::adapters::ota::Updater<bsp::FirmwareFlash>, bsp::reset::Reboot, OTA_CMD_PARKED>`):
слот — `static`, тип ему нужен, а знает его только эта сторона. Третий
аргумент — места ожидания входа `CMD`, `domain::ota::cmd_parked` от числа его
клиентов, которое считает генерация: один клиент держит вызов, остальные
ждут, и мест им на одно меньше клиентов — при умолчании `sync-request` (4)
ждущие сверх четырёх будили бы друг друга по кругу. Подключили к `CMD` своего
клиента — прибавьте его к числу в `OTA_CMD_PARKED`. Слот адаптера — `local` (фича
`supervisor/local-resources` в
`crates-cross/Cargo.toml`): адаптер `Updater` не `Send` (держит `&'static
Mutex<NoopRawMutex, …>`), и граф держит его на своём исполнителе, не требуя
`Send`. Плата за это — по `unsafe`-блоку на каждый такой `local`-`static`,
который макрос эмитит в `app`. Узел, чей `local`-слот заполняется
из `inputs`, нельзя перевести на другой
исполнитель (`executor:`): это допущение «один исполнитель», и менять его
вместе с `NoopRawMutex` у флеша (`docs/flash.md`).

У узла нет `watchdog:`, и это не забывчивость: команды может не быть всю
жизнь платы, и прогресса, о котором можно отметиться, у узла нет. Аппаратный
сторож кормит тикер графа, а не какой-то узел.
{%- endif %}
