# Первый свой узел

Узел — подсистема со своей задачей. Объявление (`supervisor_fragment!`), задача
и тесты живут в `domain`, а в `cross` от узла остаётся {% if graph == "true" %}одна запись в графе{% else %}модуль задачи,
который раздаёт ему рычаги платы и спавнит его{% endif %}. Почему так —
`docs/architecture.md`, «Узлы — фрагменты в `domain`» и следующие разделы; здесь —
путь по шагам.
{%- if graph == "true" %}

Образец — `crates-host/domain/src/heartbeat.rs`: периодический узел,
который только отмечается у сторожа. Что ещё умеет узел графа (почтовый ящик,
публикация, большие объекты в слоте) — `docs/modules/app-graph.md`.
{%- else %}
{%- capture samples %}{% if has_usb == "true" %}, узел USB — самый короткий (`crates-host/domain/src/usb.rs` и `crates-cross/app/src/tasks/usb.rs`){% endif %}{% if ota == "true" %}, узел OTA — самый полный (`crates-host/domain/src/ota.rs` и `crates-cross/app/src/tasks/ota.rs`){% endif %}{% if has_modbus == "true" %}, узел Modbus (`crates-host/domain/src/modbus.rs` и `crates-cross/app/src/tasks/modbus.rs`){% endif %}{% if has_protobuf == "true" %}, узел protobuf (`crates-host/domain/src/protobuf.rs` и `crates-cross/app/src/tasks/protobuf.rs`){% endif %}{% if has_canopen == "true" %}, узел CANopen (`crates-host/domain/src/canopen.rs` и `crates-cross/app/src/tasks/canopen.rs`){% endif %}{% endcapture %}

Графа в проекте нет: узлы спавнит `tasks::spawn_all`
(`crates-cross/app/src/tasks.rs`), по модулю на задачу. {% if samples != "" %}Образцы: {{ samples | remove_first: ", " }}.{% else %}Готового узла-образца в
проекте нет: `domain::cpu_load` без графа — не узел, а цикл. Образец фрагмента
и задачи — в репозитории шаблона
([rust-embedded-template](https://github.com/Sergycb/rust-embedded-template)):
модуль `domain::usb` — самый короткий узел.{% endif %}
Сторожа и перезапусков нет: узлы не падают, а зависший остаётся зависшим.
{%- endif %}

1. **Модуль в `domain`** — `crates-host/domain/src/<узел>.rs` и строка
   `pub mod <узел>;` в `crates-host/domain/src/lib.rs`. В модуле — фрагмент
   (`supervisor_fragment!` с `ctx: $crate::<узел>::<Узел>Ctx` и
   `task: $crate::<узел>::task`, всё внутри полным путём), задача
   `pub async fn task(ctx: <Узел>Ctx<'_>)` с циклом по смыслу узла
   (`docs/architecture.md`, «Каждая задача узла — цикл на контексте
   фрагмента»).
2. **Новое слово DSL — фича `supervisor`.** Слова фрагмента — под фичами
   `supervisor`, и все они выключены по умолчанию. Без фичи работают
   `resources:`, `inbox:` и `publish:`/`subscribe:`; включены те, что пишут
   узлы шаблона: `local` у слотов{% if graph == "true" %}, `watchdog:`{% endif %}{% if ota == "true" %}, `deps:`/`ready`,
   `request:`/`serves:`/`asks:`{% endif %}. Остальное (`flags:`, `restart:`,
   `shutdown:`{% if ota != "true" %}, `deps:`, `request:`{% endif %} и др.) не скомпилируется с именем
   нужной фичи в ошибке: допишите её в список `supervisor` в
   `crates-host/domain/Cargo.toml`. {% if graph == "true" %}Слова самого графа (блок `watchdog:`,
   `executor`) — в `crates-cross/Cargo.toml`.{% else %}API `supervisor`, которое зовёт модуль задачи
   в `app`, — фичей в `crates-cross/Cargo.toml`{% if ota == "true" %} (как `startup` ради
   `supervisor::Readiness` в `tasks/ota.rs`){% endif %}.{% endif %} Какое слово под какой фичей —
   `[features]` в `Cargo.toml` крейта `supervisor`.
{%- if graph == "true" %}
3. **Строка в графе** — в `fragments:` файла `crates-cross/app/src/graph.rs`:
   `::domain::<УЗЕЛ>_FRAG as <УЗЕЛ>`. Имя узлу даёт эта запись, а не фрагмент.
   Узел со входом (`boot: inputs: struct …`) получает его значение там же —
   `… as <УЗЕЛ> = ::domain::<узел>::<Вход> { … }`, узел с параметрами — типы в
   угловых скобках, `::domain::<УЗЕЛ>_FRAG<Тип, …> as <УЗЕЛ>` (образцы — `CPU_LOAD`{% if ota == "true" %}
   и `OTA`{% endif %} в `graph.rs`).
4. **Сторож — если узел обещает прогресс.** Поле `watchdog:` фрагмента и
   `const WATCHDOG` рядом с узлом; новый узел обязан встать в список
   `const`-assert'а в `graph.rs`, иначе цепочка таймаутов
   (`docs/watchdog.md`) не проверяется. Узел, который ждёт событие, которого
   может не быть всю жизнь платы, сторожем не отмечается{% if has_net == "true" %} — как `NET`{% if net_services contains "rest" %} и `HTTP`{% endif %}{% endif %}.
5. **Железо — через рычаг платы.** Узел не берёт периферию сам: `bsp` отдаёт
   рычаг полем `Board`, а адаптер над ним, чей тип или память выбирает
   приложение, `app` строит в `fragments:` и отдаёт узлу во входе фрагмента
   (`docs/architecture.md`, «Адаптеры делятся по одному признаку — нужен ли чип»).
6. **Тест на хосте** — по образцу `mod tests` в `crates-host/domain/src/heartbeat.rs`
   (тесты — в модуле узла, не отдельным файлом).
{%- else %}
3. **Модуль задачи** — `crates-cross/app/src/tasks/<узел>.rs`, строка
   `mod <узел>;` в `crates-cross/app/src/tasks.rs` и вызов `spawn` в
   `spawn_all`. Задача embassy не бывает generic, поэтому её аргументы —
   рычаги платы (`bsp`) и типы `app`; контекст узла задача собирает из
   локальных переменных своего кадра — по полю `&mut` на слот:

   ```ignore
   // crates-cross/app/src/tasks/sensor.rs
   #[embassy_executor::task]
   async fn task(mut bus: bsp::SensorBus) {
       domain::sensor::task(domain::sensor::SensorCtx { bus: &mut bus, index: 0 }).await;
   }

   pub fn spawn(spawner: embassy_executor::Spawner, bus: bsp::SensorBus) {
       spawner.spawn(task(bus).expect("задача SENSOR свежая: спавнится один раз"));
   }

   // в spawn_all (crates-cross/app/src/tasks.rs{% if ota != "true" and has_usb != "true" and has_modbus != "true" and has_protobuf != "true" and has_canopen != "true" %}; параметр `_board`
   // свежего проекта переименуйте в `board`{% endif %}):
   sensor::spawn(spawner, board.sensor_bus);
   ```

   Входы `request:` — `static` модуля задачи{% if ota == "true" %} (`CMD` и `ASK` в `tasks/ota.rs`){% endif %}.
4. **Железо — через рычаг платы.** Узел не берёт периферию сам: `bsp` отдаёт
   рычаг полем `Board`, а адаптер над ним, чей тип или память выбирает
   приложение, строит модуль задачи — память в его статиках — и отдаёт узлу
   (`docs/architecture.md`, «Адаптеры делятся по одному признаку — нужен ли чип»).
5. **Тест на хосте** — `mod tests` в модуле узла, не отдельным файлом{% if ota == "true" %}
   (образец — `crates-host/domain/src/ota.rs`){% endif %}.
{%- endif %}
   `supervisor::stand!` строит контекст и порты узла без {% if graph == "true" %}графа{% else %}модуля задачи{% endif %}, время
   двигает `MockDriver`. Гоняется только `cargo xtask test host` —
   `docs/conventions.md`, «Host-тесты — только через nextest».

Перед коммитом — четыре команды из `AGENTS.md`, «Порядок проверки перед
коммитом»: узел собирается и в `domain`, и в `crates-cross`.
