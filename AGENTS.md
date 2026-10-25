# AGENTS.md

Embedded-проект на STM32 (Rust, `no_std`, [Embassy](https://embassy.dev)), собранный из
шаблона [rust-embedded-template](https://github.com/Sergycb/rust-embedded-template).
Этот файл — для ИИ-сессий, работающих с репозиторием: что здесь нельзя нарушать и почему.
Человеко-читаемое описание архитектуры и таблицы зависимостей — в README.md, разделе
«Архитектура и когда что применять». Обоснование выбора каждой конкретной библиотеки — в
doc-комментариях (`//!`) над её примером использования, а для модулей `crates-cross`
(заголовочная `//!`-документация переехала) — в `docs/modules/*.md`; см. ссылки в
README — не дублируется здесь.

**Если вы в репозитории самого шаблона** (признак — каталог `chip-data-gen/` и файл
`chip-select.rhai` в корне), то этот файл описывает лишь то, что шаблон раздаёт
проектам. Механика самой раздачи — каскад выбора чипа, плейсхолдеры, генерация
`memory.x`, чем проверять изменения — в `MAINTAINING.md`; в сгенерированные проекты он
не копируется. Правило оттуда, которое стоит знать до первой правки: **в файлах шаблона
живёт Liquid**, поэтому `cross` в самом репозитории не собирается, а проверяется
генерацией (`--bin template-check`).

Общие идиомы Rust (структура workspace, naming, типы, ошибки, async cancel-safety,
embedded/`no_std` паттерны — Embassy, HAL, BSP, DMA, MPU, watchdog) покрыты установленным
skill'ом `rust-engineering` и не дублируются здесь. Этот файл фиксирует только решения,
специфичные для данного шаблона, и осознанные отступления от общих рекомендаций skill'а.

## Граница `domain`/`cross`

Два независимых workspace: `domain` (корень) — вся бизнес-логика, `no_std`, не знает про
`embassy-stm32` и ни про какое железо, тестируется на host (`cargo xtask test host`).
`crates-cross/` — аппаратная прошивка (`bsp`, `app`{% if ota == "true" %}, `boot`{% endif %}), собирается под `{{target}}`,
подключает `domain` как обычную path-зависимость.
{%- if ota == "true" %}
**Исключение — `boot`:** bootloader зависит от `bsp` (банки, HAL, константы
раскладки; фича `time-driver` ему не включается — `docs/architecture.md`; точка
входа и паникёр у него свои) и от `domain` (при `signed`{% if ota_store == "external" %} или `ext_flash=yes`{% endif %})
ради двух свободных функций над блокирующими разделами,
`domain::ota::boot::{boot_plan, screen_update}`; в образ попадают только они
(`lto` + `--gc-sections` выбрасывают остальное). Их типы — `BootPlan`, `Screening`,
`ScreenFailure` — тоже в `domain::ota::boot`. Больше `cross` в `domain` ходит только
через `app`; `bsp` от `domain` не зависит — `docs/architecture.md`.
{%- else %}
`cross` в `domain` ходит только через `app`; `bsp` от `domain` не зависит —
`docs/architecture.md`.
{%- endif %}
`cross` остаётся минимальным: `bsp` — рычаги платы (объекты под трейтами и
портами: потоки, шины, разделы флеша,{% if graph == "true" %} сторож,{% endif %} драйверы микросхем платы; DMA-буферы
под железо), `app` — оркестрация задач (`Spawner`{% if graph == "true" %}, supervisor-графы, watchdog{% endif %}) и
статики, которые домен получает ссылками (буферы узлов и транспортов{% if has_modbus == "true" %}, карта
Modbus{% endif %}{% if net_services != "" %}, секреты служб{% endif %}). Объектов `app` не строит: **адаптер над рычагом собирает
узел** через порт `ports::Assemble` (`type Source` — рычаг платы, `type Buffers`
— память и параметры от `app`; реэкспорт `domain::Assemble`) — {% if graph == "true" %}инициализатором
слота во фрагменте{% else %}в задаче{% endif %}; `app` называет тип адаптера
параметром фрагмента и раздаёт поля платы и ссылки на статики.
{%- capture wirings %}{% if has_modbus == "true" %}, `ServerWiring`{% endif %}{% if has_protobuf == "true" %}, `ProtobufWiring`{% endif %}{% if ota == "true" %}, `OtaWiring`{% endif %}{% if has_canopen == "true" %}, `CanOpenWiring`{% endif %}{% if net_services contains "ssh" %}, `SshWiring`{% endif %}{% if net_services contains "mqtt" %}, `MqttWiring`{% endif %}{% endcapture %} Входы с такой
сборкой{% if wirings != "" %} ({{ wirings | remove_first: ", " }}){% endif %} пишутся руками, а не `boot: … struct`: выведенная структура
допускает в инициализаторе только чтение поля. Порт с адаптером или библиотека
прямо в `domain` — критерий в `docs/architecture.md`.
Всё остальное — даже асинхронное и «системное» на вид (стейтчарты, RPC, синхронизация
задач) — живёт в `domain`, а не в `cross`. Узлы тоже: подсистема объявляет свой
`supervisor_fragment!`-ом{% if graph == "true" %} (`domain::cpu_load::CPU_LOAD_FRAG`){% endif %} рядом со своей задачей — самодостаточным модулем с задачей,
политикой и таймаутом, все имена внутри полным путём (`$crate::…`, `::supervisor::…`),
типы, которых он не видит, — параметрами{% if ota == "true" %} (`domain::ota::OTA_FRAG<D>`){% endif %}.
{%- if graph == "true" %} Граф собственных узлов не объявляет:
`crates-cross/app/src/graph.rs` — только `boot:`, блок `watchdog:` и `fragments:`,
который перечисляет фрагменты и даёт узлам имена (`HEARTBEAT_FRAG as HEARTBEAT`,
`CPU_LOAD_FRAG as CPU_LOAD`).
{%- endif %}
**Каждая задача узла — автомат `#[fsm::typestate]`** на контексте, который эмитит
фрагмент (`ctx: $crate::cpu_load::CpuLoadCtx, task: $crate::cpu_load::task;`): таблица на
`impl <Узел>Ctx`, действия — его методы, задача — `match ctx.run().await {}`
{%- if has_canopen == "true" %}
(состоянию, которое заимствует данные из кадра задачи, — `ctx.run(&буфер, …)`
с аргументами стартового действия, как у `domain::canopen`)
{%- endif %}
(у автомата с выходным состоянием `exit Done { result: TaskExit }` —
`ctx.run().await.result`).
Голый `loop` в задаче узла и проекция аргументов (`task: f(ctx.поле)`) не пишутся
{%- if graph == "true" %}.{% else %};
единственное исключение — `cpu_load::log_load` (не узел).{% endif %}
{%- if graph == "true" %}
Объект от килобайта в слот по значению не кладётся (дважды проходит через стек
под `flip-link`): слот `&'static mut T = static` — `docs/modules/app-graph.md`.
{%- endif %}
**Узлы не падают:** у всех `restart: Never`, `TaskExit::Failed` не возвращает ни
один. Отказ связи (канал, шина, транспорт) — запись в лог и возврат в
начальное состояние автомата, а не выход и перезапуск{% if graph == "true" %} графом{% endif %}; переждать отказ
(не вернуть ошибку снова без ожидания) — контракт порта, его держит реализация.
Отсюда и зависимость `domain` от `supervisor` (ради контекста и `TaskExit` в сигнатуре) —
`embassy-executor` приезжает туда транзитивно, и это осознанно; `embassy-stm32` — нет.
Подробности, прецеденты и пограничные случаи (например, `watchdog`) — `docs/architecture.md`.

## Команды

Рабочий каталог для всех команд ниже — корень сгенерированного проекта; `cargo xtask`
сам передаёт нужные манифесты, заходить в `crates-cross/` не надо.

### Без платы

| Команда | Что делает |
|---|---|
| `cargo xtask lint` | `domain`: `fmt --check` + `clippy -D warnings` + `cargo doc` без предупреждений |
| `cargo xtask test host` | `domain`: `cargo nextest` + doc-тесты |
| `cargo xtask lint cross` | `crates-cross`: `fmt --check` + `clippy -D warnings` |
| `cargo xtask build` | `crates-cross`: debug + release,{% if ota == "true" %} `app.bin`,{% endif %} размер образа{% if ota == "true" %}, подпись{% endif %} |
| `typos` | опечатки в коде и документации; исключения — `.typos.toml`. Нужен `typos-cli` (ставит `cargo xtask setup`), гоняется в CI отдельным джобом |
| `cargo deny check` | лицензии (только пермиссивные), источники, уязвимости зависимостей по `deny.toml`; для прошивки — с `--manifest-path crates-cross/Cargo.toml`. Нужен `cargo-deny` (ставит `cargo xtask setup`), гоняется в CI |
| `cargo xtask pins [БЛОК\|ПИН]` | справочник по чипу `{{chip}}`: выводы, альтернативные функции, DMA, тактирование |
| `cargo xtask pins --check` | не занят ли отладочный порт в `resources.rs` |

### С платой — не запускать без явного разрешения пользователя

| Команда | Что делает |
|---|---|
| `cargo xtask setup` | ставит `rustup target` и компоненты, `probe-rs`, `flip-link`, `cargo-nextest`, `cargo-deny`, `cargo-hack`, `cargo-llvm-cov`, `cargo-insta`, `typos-cli`, а для ручного запуска — `cargo-mutants`, `cargo-bloat`, `cargo-machete`, `taplo-cli` — меняет систему |
| {% if ota == "true" %}`cargo flash -p boot --release --chip {{chip}}`, затем {% endif %}`cargo run -p app` (исключение из правила выше: запускать в `crates-cross/`) | прошивка руками — необратимо; отдельной команды в `xtask` нет, и начало образа с `memory.x` здесь не сверяется — это делает `cargo xtask build` |
| `cargo xtask test target` | тесты внутри МК (`embedded-test` + `probe-rs run`){% if ota == "true" %}; сам заливает bootloader{% endif %} |
| `cargo xtask test host-target` | хост управляет прошитым устройством; сам заливает release-образы, сверив начало приложения с `ORIGIN` раздела из `memory.x` |
| `cargo xtask test all` | обе строки выше |
| `cargo xtask panic-info` | читает регион `PANIC` с живой платы через `probe-rs` |
| `probe-rs attach --chip {{chip}} crates-cross/target/{{target}}/debug/app` | лог живой платы |
| `probe-rs erase --chip {{chip}} --connect-under-reset` | снять плату, зациклившуюся на панике |

### Порядок проверки перед коммитом

```
cargo xtask lint          # domain: fmt --check + clippy -D warnings + cargo doc
cargo xtask test host     # domain: cargo nextest + doc-тесты

cargo xtask lint cross    # cross: fmt --check + clippy -D warnings
cargo xtask build         # cross: debug + release
```

Это основа CI — там host и cross разнесены по разным джобам, которые идут
параллельно и с раздельными кешами. Сверх этих четырёх CI гоняет `cargo deny
check`, `cargo hack --feature-powerset`, покрытие (`cargo llvm-cov`) и `typos`:
все четыре ставит `cargo xtask setup`, и при правке зависимостей или фич первые
три стоит прогнать и локально. Отдельно от `ci.yml` (GitHub): `pins.yml` (Pins) —
`chip-info --check` по фильтру путей (распиновка и справочник){% if ota == "true" %}, `release.yml`
(Release) — `cargo xtask build` и подпись по тегу `v*`{% endif %}. Полный
перечень и что нужно для зелёного CI — README, «Что проверяет CI». Добавили шаг
в CI — допишите его сюда, компилятор такого расхождения не увидит.

Lock-файлы проект считает сам: корневой `Cargo.lock` создаёт первая
host-сборка, `crates-cross/Cargo.lock` — первая cross-сборка,
`crates-host/chip-info/Cargo.lock` — первый `cargo xtask pins`. **Их надо
закоммитить**, а не принять за мусор сборки. Шаблон их не раздаёт намеренно:
состав пакетов зависит от ответов при генерации, и один шаблонный lock был бы
верен одному сочетанию, а остальным ронял бы `cargo fetch --locked` в CI.

Первые две команды железа не требуют вовсе, третья и четвёртая — только тулчейн с
целевым target'ом (ставится `cargo xtask setup`). Два оставшихся этапа тестирования
нужны с подключённой платой:

```
cargo xtask test target        # тесты внутри МК (embedded-test + probe-rs run)
cargo xtask test host-target   # хост управляет прошитым устройством
```

В `xtask` намеренно только то, что не делается одной командой `cargo`: две сборки
подряд, два воркспейса с разными наборами флагов и этапы тестирования (вместе с
прошивкой перед ними{% if ota == "true" %} в правильном порядке — сначала bootloader{% endif %}). Обёрток над `probe-rs` (`run`, `attach`,
`size`, `reset`, `erase`) здесь нет и не надо: они ничего не добавляли к прямому
вызову, а `probe-rs` в проекте всё равно обязателен.

`cargo xtask panic-info` этому правилу не противоречит, хотя внутри и зовёт `probe-rs
read`: руками пришлось бы сначала найти адрес региона `PANIC` в `crates-cross/app/memory.x`,
потом знать формат `panic-persist` (магия `0x0FACADE0`, длина, байты) и собирать
строку из hex-вывода. Команда делает ровно это — не удаляйте её как «ещё одну
обёртку». То же и с `cargo xtask pins`: путь к манифесту `chip-info` относительный,
прямой вызов работал бы только из корня проекта. Смотреть лог живой платы —
`probe-rs attach --chip <CHIP> <elf>`, вытащить зациклившуюся на панике —
`probe-rs erase --chip <CHIP> --connect-under-reset`.

## Запреты

- Не прошивать плату (`cargo flash`, `cargo run` в `crates-cross`, `test target`,
  `test host-target`, `panic-info`, `probe-rs`) без явного разрешения
  пользователя — необратимо и требует железа.
- Не добавлять зависимости без разрешения пользователя — правила см. `docs/conventions.md`.
- Не пинить тулчейн — `rust-toolchain.toml` в шаблоне намеренно нет, см. `docs/conventions.md`.

## Инварианты, которые не ловит компилятор

- Задачи `embassy` не могут быть generic: тип, уезжающий в `#[embassy_executor::task]`,
  получает псевдоним (`pub type X = ...`): рычаг — от `bsp`{% if ota == "true" or config == "true" %} ({% if ota == "true" %}`bsp::FirmwareFlash`{% if config == "true" %},
  {% endif %}{% endif %}{% if config == "true" %}`bsp::ConfigFlash`{% endif %}){% endif %}, адаптер — от `app`{% if protocol_link == "true" %} (`crate::link::<вид>::<транспорт>::Link`){% endif %} или
  конкретным типом прямо {% if graph == "true" %}в графе{% else %}в задаче{% endif %}{% if ota == "true" %} (`adapters::ota::Updater<bsp::FirmwareFlash, _>`){% endif %}
  — `docs/architecture.md`.
- Ровно один `#[panic_handler]` и один активный `#[global_logger]` на бинарник: логгер
  резолвится линковкой по имени символа и даёт `multiply defined!` при нарушении;
  паникёр у каждого бинарника свой: у `app` — `crates-cross/app/src/panic.rs`,
  выбирается `#[cfg(debug_assertions)]`: dev — `use panic_probe as _;`, release —
  свой `#[panic_handler]` поверх `panic_persist::report_panic_info` (фича
  `custom-panic-handler`);{% if ota == "true" %} у `boot` — сброс МК без сообщения (`boot/src/main.rs`: на сбросе держится продолжение прерванного обмена);{% endif %} у
  `target-tests` — `embedded-test` — `docs/diagnostics.md`, `docs/conventions.md`.
{%- if ota == "true" %}
- `prepare(len)` порта `FirmwareUpdate` зовётся один раз перед приёмом образа
  (внутри `domain::ota::download::Transfer::start`), `write()` сектор больше не
  стирает — `docs/ota.md`.
{%- endif %}
- `lib.rs` только объявляет: атрибуты, `//!`, `mod`, `pub use` — определения
  живут в модулях — `docs/conventions.md`.
{%- if ota == "true" %}
- Образ приходит командами (`ports::OtaCommand`) на вход `CMD` узла OTA —
  запрос с ответом — или на `ASK` (`serves:`; клиент не ждёт исхода в вызове и
  забирает его из `reply()`), и каждая команда получает исход (`ports::OtaReply`), который
  протокол отдаёт хосту в своём виде ({% if has_protobuf == "true" %}protobuf — `Ok`/`Error`, {% endif %}{% if has_modbus == "true" %}Modbus — код `ports::reply_code`, {% endif %}{% if has_canopen == "true" %}CANopen — код `ports::reply_code`, {% endif %}`docs/ota.md`). Своего канала у OTA нет: узел протокола
  только переводит сообщения в команды. Запрос несёт номер клиента —
  транспорта (`domain::ota::OtaClient`, номер из генерации): приём держит один
  владелец, чужим `Start`/`Chunk`/`Finish` — `Busy` до `Finish`, отказа или
  срока молчания. МК узел сбрасывает только по команде
  `Reboot` (порт `ports::Reboot`, `bsp::reset::Reboot`), не по `Finish`: хост
  узнаёт исход до сброса. Отказы до стирания обязаны оставаться такими:
  `Busy` и отсутствие подписи — до `Transfer::start`, длина — внутри него, до
  `prepare` — `docs/ota.md`.
{%- endif %}
{%- capture mutexes %}{% if shared_flash == "true" %}, `bsp::flash::FlashMutex`{% endif %}{% if ota == "true" and ota_store != "external" %}, мьютексов носителей `DFU` и `STATE`
  (`bsp::flash::DFU_FLASH`, `bsp::flash::STATE_FLASH`, их ждёт `adapters::ota::Linked`{% if dfu_bank == "true" %}; `DFU` во втором банке —
  `bsp::flash::DfuBankMutex`{% endif %}){% endif %}{% if config_store == "flash" %}, `CONFIG` (`bsp::flash::CONFIG_FLASH`){% endif %}{% if external_flash == "true" %},
  внешней флеши (`bsp::external::ExternalMutex`){% endif %}{% if ota == "true" %}, `local` на слотах узла OTA
  (`domain::ota`){% endif %}{% endcapture %}
{%- if mutexes != "" %}
- `NoopRawMutex` у {{ mutexes | remove_first: ", " }} — одно допущение «один исполнитель», менять вместе —
  `docs/flash.md`, `docs/architecture.md`.{% if has_modbus == "true" %} Карта регистров Modbus
  (`domain::modbus::MapMutex`) в него не входит: она в `OnceLock` в `static`
  (`crate::modbus::handle()`), и мьютекс — `CriticalSectionRawMutex` ради `Sync`.{% endif %}
{%- endif %}
{%- if ota == "true" %}
- Подтверждение образа (`mark_booted`) — в стартовом действии узла OTA
  (`domain::ota`, `begin`, не удалось — ещё раз на следующем `Start`) и один
  раз за запуск (слот `CONFIRMED`). Не
  возвращать его в `main` до исполнителя{% if ota_store == "external" %}: драйвер внешней флеши может ждать
  таймер{% endif %}. Не звать после принятого образа (`mark_updated` ставит тот же
  флаг): после обмена и до него `STATE` одинаково `Swap`, и повтор снял бы
  принятое обновление{% if has_usb == "true" %}. Узел `USB` встаёт на шину только после этой попытки
  ({% if graph == "true" %}`ready` узла OTA и `deps: [OTA ready]` у `USB_FRAG`{% else %}сигнал
  `READY_<транспорт>` в `crates-cross/app/src/tasks/ota.rs`{% endif %}): подтверждение после обновления или отката стирает
  `STATE`, ядро стоит на выборке, и перечисление, попавшее на это, хост
  бракует до переподключения —{% else %}. Подробности —{% endif %} `docs/ota.md`.
{%- endif %}
{%- if ota == "true" and ota_store == "external" %}
- С `ext_flash=yes` (`DFU`/`STATE` на внешней флеши): `EXT_PAGE_SIZE` — одно
  число из генерации — переменная среды `[env]` в
  `crates-cross/.cargo/config.toml`, общая у `boot` и `bsp::external`, — и стирание
  драйвера обязано его делить (`const`-assert в обоих); драйвер —
  два слота одной микросхемы (блокирующий в `boot`, асинхронный в `bsp`).
  Bootloader загружает текущий образ без обмена, только если `STATE` не
  читается (`BootPlan::BootCurrent`) или (с подписью) новый образ отвергнут; при
  читаемом `STATE` (`BootPlan::Prepare`) — строгий `prepare`, `try_prepare` не
  используется, прыжок в наполовину переписанный `ACTIVE` недопустим — `docs/flash.md`, `docs/ota.md`.
{%- endif %}
{%- if swap_paged == "true" %}
- `SWAP_PAGE_SIZE` — число из генерации ({{page_size}}), переменная среды
  `[env]` в `crates-cross/.cargo/config.toml`; константа — `bsp::flash::SWAP_PAGE_SIZE`,
  её берёт и `boot`:
  страница обмена крупнее физической, `ACTIVE` и `DFU`
  оборачиваются в `ErasePage<_, SWAP_PAGE_SIZE>` ради малого журнала `STATE`
  (подписанному `boot` нужна зона ≥ 26 KiB). `STATE_ERASE_SIZE` — другое число
  (страница сектора `STATE`) — `docs/flash.md`.
{%- endif %}
{%- if ota == "true" and ota_store != "external" %}
- `STATE_ERASE_SIZE` — число из генерации, одно на `boot` и приложение (переменная
  среды в `crates-cross/.cargo/config.toml`, константа — `bsp::flash::STATE_ERASE_SIZE`). {% if state_paged == "true" %}Здесь `BOOTLOADER_STATE` строится над
  `flash_page::ErasePage` со страницей своего сектора: над цельным `Flash` на F2/F4/F7 раздел не
  кратен его странице, и `Partition::new` паникует на старте.{% else %}Здесь сектор `STATE` равен
  странице цельного `Flash`, поэтому раздел строится прямо над ним, а равенство держит
  `const`-assert рядом с константой (`bsp::flash`).{% endif %}{% if config_store == "flash" %} Так же и `CONFIG`
  (`CONFIG_ERASE_SIZE` в `bsp::flash`): он может лежать в мелких секторах перед
  `ACTIVE` —{% else %} Подробности —{% endif %} `docs/flash.md`.
{%- endif %}
{%- if ota == "true" %}
- Флеш под обновлением плата отдаёт не нарезанным (`board.firmware:
  bsp::FirmwareFlash` = `adapters::ota::Linked`: носители `DFU`/`STATE` со своими
  страницами и размер `ACTIVE`); разделы по символам `memory.x`
  (`FirmwareUpdaterConfig::from_linkerfile`) находит узел OTA, собирая адаптер
  (порт `ports::FirmwareLayout`). Нарезка стоит под `cfg(target_os = "none")`:
  host-lint и host-тесты её не видят (там вместо неё — готовые
  `ports::FirmwarePartitions`), проверяет её только `cargo xtask lint cross`/`build` —
  `docs/ota.md`.
{%- endif %}
{%- if bank_switch != "" %}
- Флеш этого чипа с завода двухбанковый, а раскладка одобанковая:
  `bsp::banks::ensure_single_bank()` стоит первой строкой {% if ota == "true" %}и в `boot`, и в
  `Board::new`{% else %}`Board::new`{% endif %} — до любого `Flash::new`. Не переносить ниже и не убирать:
  `embassy-stm32` паникует на несовпадении режима (G0/G4) или стирает не те
  страницы (L4+). Новая плата прошивается дважды: первая прошивка
  переключает режим и перезагружает МК — `docs/flash.md`.
{%- endif %}
- Паника и фолт — решение прошивки, а не рычаг платы: паникёр, обработчик
  `HardFault` с причиной и ловушка деления — в `app` (`crates-cross/app/src/panic.rs`,
  `crates-cross/app/src/fault.rs`; ловушку `main` включает первой строкой). Не
  переносить обработчик в `bsp`: у `#[exception]` экспортируемое имя, и с LTO он
  попал бы в любой бинарник, линкующий `bsp`{% if ota == "true" %}, — в `boot` это около 14 KiB зоны{% endif %}
  (`docs/clocks.md`, «Ядро», `docs/architecture.md`). Сброс МК — рычаг
  `bsp::reset::Reboot`: на M7 он сначала сбрасывает кэш данных в RAM, иначе дамп
  паники пропадёт.
{%- if core_baseline != "true" %}
- `Board::cycles` (`bsp::cycles::Cycles`, порт `ports::CycleCounter`) включает
  `DWT.CYCCNT`; `DWT` и `DCB` `bsp` берёт через `steal()` и пишет только в
  `Cycles::new` — приложение берёт свой `take()` как раньше.
{%- endif %}
{%- if cache != "" %}
- `bsp::cache::enable()` — в `Board::new` сразу за `embassy_stm32::init`: кэши
  с завода выключены, HAL их не включает. Что именно включается, решает
  генерация по метаданным чипа (`docs/clocks.md`, «Кэши»).
{%- if cache contains "scb_d" %}
  Кэш данных ядра M7 DMA не видит, поэтому он включён только вместе с
  non-cacheable регионом `DMA_NC` (`memory.x`, `DMA_NC_START`/`DMA_NC_LEN` в
  `[env]`){% if bdma_nc_start != "" %} и `SRAM4` под BDMA{% endif %}, который
  `bsp::dma_nc` размечает в MPU: буфер DMA кладите в `#[link_section = ".dma_nc"]`{% if bdma_nc_start != "" %} (BDMA — `".bdma_nc"`){% endif %}, не в обычный `static`. Секции в
  `memory.x` вставлены `INSERT AFTER .gnu.sgstubs` — за `.uninit` они ломают
  `cortex-m-rt` под `flip-link`.
{%- endif %}
{%- endif %}
{%- if cpu_freq_boost == "true" %}
- `bsp::clocks::ensure_cpu_freq_boost()` — первой строкой `Board::new`, до
  `embassy_stm32::init`: option byte `CPU_FREQ_BOOST` (потолок ядра 550 МГц
  вместо 520) читается при `init()` и вступает в силу только сбросом. Бит
  пишется один раз на кристалл, побочно отключает ECC на ITCM/DTCM —
  `docs/clocks.md`.
{%- endif %}
{%- if has_net == "true" %}
{%- if rng_irq != "" %}
- Клок блока `RNG` даёт `config()` в `crates-cross/bsp/src/board.rs` — у
  каждой линии свой источник (PLL Q, HSI48, `RNGSEL`). Меняете тактирование —
  сохраните его: без клока `Rng::new` в `Board::new` паникует (линии с
  мультиплексором RNG) или прошивка виснет в ожидании слова там же, в
  `Board::new` (F2/F4/F7, L0, L4, U0) —
  `docs/modules/bsp-rng.md`. Блок `RNG` берёт группа `rng` из
  `resources.rs`, и генератор наружу не выходит: `bsp::rng::seed` в
  `Board::new` засевает им сетевой стек и {% if net_services contains "ssh" %}оставляет его бэкенду
  `getrandom` (`bsp::rng`, символ `__getrandom_v03_custom`; бэкенд выбирает
  `--cfg getrandom_backend="custom"` в `crates-cross/.cargo/config.toml`) —
  другого пути к генератору нет, SSH берёт байты через `getrandom`{% else %}дропает его, так что блок
  включён только на время затравки{% endif %}.
  Блок и клок заводятся только с сетевым транспортом.
{%- endif %}
{%- endif %}
{%- if protocol == "canopen" %}
- Буфер сохранённых объектов CANopen и колбэки стека над ним — локальные
  `domain::canopen::task`, автомат получает их аргументами стартового действия
  (`start(..) => begin` в таблице, `ctx.run(&persisted, &mut callbacks)`), а
  `Node<'f>` живёт в состояниях. Не переносить их в `static` или слот: тип
  замыкания там не назвать, а колбэки `zencan` своих данных не получают.
  Ключи `OBJECTS_KEY` и `NODE_ID_KEY` хранилища настроек заняты узлом{% if config == "true" %}, и носитель
  (`board.config`) забирает он — хранилище над ним собирает сам узел
  (`Assemble`), и своим настройкам проекта нужен общий доступ через него{% endif %} —
  `domain::canopen`.
{%- endif %}
{%- if dfu_bank == "true" %}
- `DFU` — во втором банке, который стоит за разрывом в адресах: `boot` и
  `bsp` пишут его через `Bank2Region`, а символы `__bootloader_dfu_*` в
  `memory.x` отсчитаны от начала банка 2, а не от базы flash. Менять только
  вместе; цельный `Flash` туда не дотягивается (`FLASH_SIZE` — сумма банков) —
  `docs/flash.md`.
{%- endif %}
{%- if config == "true" %}
- Хранилище настроек — `adapters::settings` над любым async `NorFlash`, на весь
  его объём. Носитель отдаёт плата рычагом `NorFlash`
  (`board.config: bsp::ConfigFlash`): {% if config_store == "eeprom" %}EEPROM L0/L1 (доступ — порт `ports::EepromAccess`, обёртку
  `adapters::eeprom::Eeprom` собирает сама плата){% elsif config_store == "external" %}внешняя флеш W25Q (драйвер
  `w25q` в `bsp::external`, шина — заглушка `bsp::stub::FlashBus`, пока проект не
  подставит выводы){% else %}раздел `CONFIG` внутреннего
  флеша{% endif %}. Само хранилище собирает узел-потребитель (`Assemble`) над
  `board.config`; тип — `adapters::settings::Settings<bsp::ConfigFlash>` прямо {% if graph == "true" %}в
  графе{% else %}в задаче{% endif %}{% if config_store == "eeprom" %}. EEPROM под хранилищем инвертирована: без
  этого заводские нули выглядели бы для `sequential-storage` закрытыми
  страницами{% endif %} — `docs/flash.md`.
{%- endif %}
{%- if graph == "true" %}
- У каждого узла с `watchdog:` своя связанная цепочка таймаутов (период
  отметок узла < `domain::heartbeat::WATCHDOG` и `domain::cpu_load::WATCHDOG`,
  и обе < `bsp::wdg::HW_TIMEOUT_US` минус `WATCHDOG_CHECK_EVERY` графа) —
  менять только вместе; левое звено держит `const`-assert в своём фрагменте,
  правое — `const`-assert в `crates-cross/app/src/graph.rs`, и новый узел с
  `watchdog:` обязан встать в его список — `docs/watchdog.md`.
- Сторож запускается в прологе `spawn_all` (`= board.watchdog.arm()`), а не в
  `Board::new`: с этого момента железо тикает, а кормит его только тикер графа.
  {%- if ota == "true" %}
  В release его раньше запускает bootloader — только перед самым прыжком в
  образ (обмен разделов и проверка подписи идут без сторожа), с
  тем же `HW_TIMEOUT_US` (число в `[env]` crates-cross/.cargo/config.toml у
  обоих общее), и путь от прыжка до
  `spawn_all` обязан в него уложиться
  {%- endif %} — `docs/watchdog.md`.
{%- elsif ota == "true" %}
- Сторожа нет ни в `boot`, ни в приложении: зависший до `mark_booted` образ сам
  не откатится.
{%- endif %}
- Цикл исполнителя в `crates-cross/app/src/main.rs` поднят руками, а не
  `#[embassy_executor::main]`: только так измеряется сон задач, на котором стоит
  узел `CPU_LOAD`. Возврат к макросу молча ломает измерение (счётчик сна
  перестаёт расти, узел публикует 100%), и ничем, кроме линтера, это не
  ловится — `docs/modules/app-cpu-load.md`.
{%- if protocol_link == "true" %}
{%- capture sources %}{% if has_uart == "true" %}, потоком UART{% endif %}{% if has_usb == "true" %}, потоком CDC{% endif %}{% if has_net == "true" %}, сетевым стеком{% endif %}{% endcapture %}
- Канал протокола (кадрирование, сокет) собирает узел протокола через
  `Assemble` над тем, что отдала плата: {{ sources | remove_first: ", " }}.
  {%- if has_modbus == "true" %} Узел Modbus slave — `domain::server::SERVER_FRAG<L, H>` ({% if graph == "true" %}граф называет
  узлы{% else %}узлы{% endif %} `MODBUS_<транспорт>`).{% endif %}
  {%- if has_protobuf == "true" %} Узел protobuf хоста — `domain::protobuf::PROTOBUF_FRAG<L, S>`
  (`PROTOBUF_<транспорт>`).{% endif %} Тип канала (`Link`), его статики и `buffers()` —
  `crates-cross/app/src/link/<вид>.rs`, по модулю на транспорт: размер буфера и
  порт — решение приложения, и `bsp` их не знает. Буфер кадра держит сам канал
  (`ports::FrameLink::buffer`), протокол — обработчик ({% if has_modbus == "true" %}`ports::Handler`:
  `domain::modbus::Slave`{% if has_protobuf == "true" %}; {% endif %}{% endif %}{% if has_protobuf == "true" %}`ports::ProtobufService`: `domain::protobuf::Service`{% endif %}).
  {%- if has_protobuf == "true" %} {% if ota == "true" %}Узел
  `PROTOBUF` ждёт кадр наперегонки с исходом команды OTA, поэтому его канал{% else %}Канал узла `PROTOBUF`{% endif %} —
  `ports::FrameDuplex`: брошенный `recv` не теряет кадр{% if cobs_used == "true" %} (у `adapters::cobs::Cobs`
  декодер живёт в канале, а не в future){% endif %}, а ответ уходит из своего буфера узла
  (`response()` модуля `link`) через `send_from`.{% endif %} Вход узла — {% if graph == "true" %}выражение в
  `fragments:` графа{% else %}`spawn` модуля задачи
  (`crates-cross/app/src/tasks/`: {% if has_modbus == "true" %}`domain::server::serve`{% if has_protobuf == "true" %}, {% endif %}{% endif %}{% if has_protobuf == "true" %}`ProtobufCtx` руками{% endif %}):
  по модулю на задачу, `main.rs` только зовёт `tasks::spawn_all`{% endif %}.
{%- endif %}
{%- if net_services contains "rest" %}
- Токен REST API — `rest-token.txt` в корне проекта, его вшивает
  `include_str!` (`crate::net::rest::TOKEN` в `crates-cross/app/src/net/rest.rs`).
  Файл создаёт `cargo xtask` (`secrets::ensure` — в `build`, `lint cross` и перед
  прошивкой), и он **коммитится**, а не уходит в `.gitignore`: токен и так открыт
  в каждом образе, а без файла не собрались бы свежий клон и CI. Число
  соединений — `HTTP_CONNECTIONS` в `[env]` `crates-cross/.cargo/config.toml`
  (`crate::net::rest::CONNECTIONS`);{% if graph == "true" %} пул `HTTP[{{ http_connections }}]` в `graph.rs` —
  литерал, который генерация пишет тем же числом (`const`-assert там же), и{% endif %}
  сокеты стека (`NET_SOCKETS` в том же `[env]`, `bsp::net`) генерация считает с ним —
  правите руками одно — правьте и остальные.
{%- endif %}
{%- if net_services contains "ssh" %}
- Консоль SSH (`domain::ssh`, узел `SSH`): {% if ssh_auth == "password" %}пароль — `ssh-password.txt`{% else %}открытый ключ пользователя — строка `crate::net::ssh::LOGIN`{% endif %}{% if ssh_host_key == "file" %}, ключ хоста — `ssh-host-key.bin` (`crate::net::ssh::HOST_KEY`){% endif %}.
  {% if ssh_auth == "password" or ssh_host_key == "file" %}Файлы в корне проекта создаёт `cargo xtask` (`secrets::ensure`) и они
  **коммитятся**{% if net_services contains "rest" %}, как токен REST{% endif %}: они вшиты и открыты в образе. {% endif %}{% if ssh_host_key == "config" %}Ключ хоста плата
  создаёт при первом старте (`domain::ssh::Stored`, ключ настроек
  `domain::ssh::HOST_KEY_ID`) — хранилище настроек забирает узел `SSH`, своим
  настройкам проекта нужен общий доступ через него. {% endif %}Случайность
  `sunset` берёт через `getrandom` (`crates-cross/bsp/src/rng.rs`): без бэкенда
  или с нулями вместо RNG ключи сессий предсказуемы. Код `sunset` — около
  165 KiB, отсюда порог раздела приложения в генерации (`SSH_MIN_PARTITION`).
{%- endif %}
{%- if net_services contains "mqtt" %}
- Брокер MQTT (`domain::mqtt`, узел `MQTT`): адрес, вход и префикс тем — `mqtt.toml` в
  корне проекта. Файл создаёт `cargo xtask` (`secrets::ensure`), он **коммитится**, а
  читает и вшивает его `crates-cross/app/build.rs` (`crate::net::mqtt::BROKER`,
  `PREFIX`; пароль открыт в образе). Заготовку сборка не отвергает: адрес
  вписывает человек. Узел подключается сам: `dial` — одна попытка, неудачная —
  строка в лог с адресом, и после неё и после любого конца сессии узел сразу
  зовёт `dial` снова; паузы перед попытками (1 → 30 с) — контракт порта
  `ports::dial::Dial`, их держит `adapters::net::TcpClient`, а не `domain`.
  {%- if ota == "true" %} Клиент входа `CMD` узла OTA для `cmd/reboot` — номер
  `{{ mqtt_ota_client }}` (`domain::ota::ClientId`).{%- endif %} Сокет клиента — слот в `NET_SOCKETS`
  (`[env]`, считает генерация); клиента собирает узел (`Assemble`) на стеке платы,
  буферы TCP — статики `crate::net::mqtt::buffers()`. Доступ к `cmd/` токеном в
  теле не защищён — только ACL брокера{% if ota == "true" %} (при пустом `user` `build.rs` предупреждает){% endif %} —
  `docs/modules/app-mqtt.md`.
{%- endif %}
{%- if graph == "true" %}
- `#![allow(dead_code)]` в начале `crates-cross/app/src/graph.rs` — не
  украшение, а условие `-D warnings`: файл целиком генерируемый, а
  `*_receiver()`/`provide_*`/`*_take()` в сыром шаблоне никто не зовёт, потому
  что подписчиков нет. Снимать allow нельзя, пока у графа нет читателей; писать
  в `graph.rs` свою логику — тоже.
{%- endif %}
- `Board` отдаёт рычаги платы — объекты под трейтами (`embedded-io-async`,
  `embedded-storage`, порты `ports` там, где стандартного трейта нет{% if graph == "true" %},
  `HardwareWatchdog`{% endif %}): потоки, шины, куски флеша{% if ota == "true" or config == "true" %} ({% if ota == "true" %}`firmware`{% if config == "true" %}, {% endif %}{% endif %}{% if config == "true" %}`config`{% endif %}){% endif %}, драйверы
  микросхем платы, данные о чипе (`bsp::ChipSpec`), — и ничего не настраивает:
  ни периферии ядра, ни `Peripherals`, ни `Clocks` в полях нет. Протоколов над
  рычагами и их буферов в `bsp` нет: тип и статики — в `app`{% if protocol_link == "true" or net_services != "" %} ({% if protocol_link == "true" %}`link`{% if net_services != "" %}, {% endif %}{% endif %}{% if net_services != "" %}`net`{% endif %}){% endif %}{% if ota == "true" or config == "true" %} или тип прямо {% if graph == "true" %}в графе{% else %}в задаче{% endif %}
  ({% if ota == "true" %}адаптер обновления{% if config == "true" %}, {% endif %}{% endif %}{% if config == "true" %}хранилище{% endif %}){% endif %}, собирает узел{% if has_stub == "true" %}. Заглушка периферии до драйвера — `bsp::stub`{% endif %} — `docs/architecture.md`.
{%- if external_flash == "true" %}
- Внешняя флеш — W25Q, драйвер `w25q` (hw-drivers, `rev` закреплён) в
  `bsp::external`; шина под ним — заглушка `bsp::stub::FlashBus`, пока плата
  не подставит свою (рецепт — `docs/modules/bsp-external.md`).
  `Board::new` асинхронный: драйвер опознаёт микросхему (`init`, до 2 с, если
  она дотирает стирание), и `main` зовёт его через `embassy_futures::block_on`
  до исполнителя{% if graph == "true" %} — путь до `spawn_all` обязан уложиться в таймаут сторожа{% endif %}.
  Не прошёл `init` или микросхема меньше `EXT_FLASH_END` — строка в лог, флеш
  отказывает, а не паникует.
{%- endif %}
- `Board::new()` отдаёт один `Board`.
  {%- if has_usb == "true" or has_net == "true" %} Пары, которые нельзя собрать порознь,
  собирает плата: {% if has_usb == "true" %}USB — `board.<транспорт>: bsp::usb::<транспорт>::Usb { device,
  cdc }` (классы регистрируются до постройки устройства){% if has_net == "true" %}, {% endif %}{% endif %}{% if has_net == "true" %}сеть — `board.net:
  bsp::net::Net { runner, stack }` (один `embassy_net::new`; сокетов —
  `NET_SOCKETS` в `[env]`, считает генерация){% endif %}.{% endif %} {% if graph == "true" %}Граф берёт `boot: board:
  bsp::Board;` и делит по полям{% else %}Поля делит `tasks::spawn_all(spawner, board)`{% endif %}.
  {%- if has_usb == "true" or has_net == "true" %}
  Не передавать половину пары через `static`{% if has_usb == "true" %} (поток CDC не `Send`){% endif %} и не делать
  поля `Board` `Option`{% else %} Не делать поля `Board` `Option`{% endif %} — `docs/architecture.md`.
{%- capture wrappers %}{% if has_usb == "true" %}, обёртки USB CDC{% endif %}{% if has_net == "true" %}, `adapters::net::Net`{% endif %}{% if config_store == "eeprom" %}, EEPROM{% endif %}{% endcapture %}
{%- capture levers %}{% if wrappers != "" %}, `adapters` ({{ wrappers | remove_first: ", " }}){% endif %}{% if has_usb == "true" %}, `embassy-usb`{% endif %}{% if has_net == "true" %}, `embassy-net`{% endif %}{% if net_services contains "ssh" %}, `getrandom`{% endif %}{% endcapture %}
- `bsp` зависит от `ports`{% if flash_page == "true" %}, `flash-page` (страница раздела{% if ota == "true" %}, общая с `boot`{% endif %}){% endif %}, HAL
  с трейтами драйверов{% if levers != "" %} и того, из чего собирает свои рычаги: {{ levers | remove_first: ", " }}{% endif %}. От `domain` — нет: типы домена{% if ota == "true" %} — например вход узла
  OTA —{% endif %} заполняет из полей `Board` compose-site в `app` — `docs/architecture.md`.
- Liquid в корневом workspace (`domain`, `ports`, `adapters`{% if protocol == "protobuf" %}, `proto`{% elsif protocol == "canopen" %}, `canopen`{% endif %},
  корневой `Cargo.toml`) пишется внутри комментариев (строки `//` и `#` с тегами `if`/`endif`): в
  репозитории шаблона эти крейты компилируются целиком, в проекте Liquid вырезает
  невыбранное — `docs/architecture.md`.
- Новую зависимость для `cross` нельзя считать рабочей на тёплом кеше — проверять
  `rm -rf target && cargo build --target {{target}} ...` —
  `docs/conventions.md`.

## Карта документации

«Почему так решено» — по темам, не здесь: `docs/README.md`.

## Перед тем как отчитаться

1. `cargo xtask lint`
2. `cargo xtask test host`
3. `cargo xtask lint cross`
4. `cargo xtask build`

Не сообщать о завершении задачи, пока все четыре не прошли (`EXIT=0`) — по одной: так видно,
какая именно упала.
