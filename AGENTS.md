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
`crates-cross/` — аппаратная прошивка (`bsp`, `app`, `boot`), собирается под `{{target}}`,
подключает `domain` как обычную path-зависимость.
**Исключение — `boot`:** bootloader зависит от `bsp` (точка входа, ядро, банки, HAL,
константы раскладки, паникёр; фичи `time-driver` и `fault-handler` ему не включаются —
`docs/architecture.md`) и от `domain` (при `signed` или `ota=external`)
ради двух свободных функций над блокирующими разделами,
`domain::ota::boot::{boot_plan, screen_update}`; в образ попадают только они
(`lto` + `--gc-sections` выбрасывают остальное). Их типы — `BootPlan`, `Screening`,
`ScreenFailure` — тоже в `domain::ota::boot`. Больше `cross` в `domain` ходит только
через `app`; `bsp` от `domain` не зависит — `docs/architecture.md`.
`cross` остаётся минимальным: `bsp` — объекты платы (периферия под трейтами, драйверы,
разделы флеша, DMA-буферы под железо), `app` — сборка адаптеров над ними с их
буферами и оркестрация задач (`Spawner`, supervisor-графы, watchdog).
Всё остальное — даже асинхронное и «системное» на вид (стейтчарты, RPC, синхронизация
задач) — живёт в `domain`, а не в `cross`. Узлы графа тоже: подсистема объявляет свой
`supervisor_fragment!`-ом рядом со своей задачей — самодостаточным модулем с задачей,
политикой и таймаутом, все имена внутри полным путём (`$crate::…`, `::supervisor::…`),
типы, которых он не видит, — параметрами (`domain::heartbeat::HEARTBEAT_FRAG`,
`domain::ota::OTA_FRAG<D>`). Граф собственных узлов не объявляет:
`crates-cross/app/src/graph.rs` — только `boot:`, блок `watchdog:` и `fragments:`,
который перечисляет фрагменты и даёт узлам имена (`HEARTBEAT_FRAG as HEARTBEAT`,
`CPU_LOAD_FRAG as CPU_LOAD`).
**Каждая задача узла — автомат `#[fsm::typestate]`** на контексте, который эмитит
фрагмент (`ctx: $crate::heartbeat::HeartbeatCtx, task: $crate::heartbeat::task;`): таблица на
`impl <Узел>Ctx`, действия — его методы, задача — `match ctx.run().await {}`
(состоянию, которое заимствует данные из кадра задачи, — `ctx.run(&буфер, …)`
с аргументами стартового действия, как у `domain::canopen`)
(у автомата с выходным состоянием `exit Done { result: TaskExit }` —
`ctx.run().await.result`).
Голый `loop` в задаче узла и проекция аргументов (`task: f(ctx.поле)`) не пишутся;
единственное исключение — `cpu_load::task` (проект без графа, не узел).
Объект от килобайта в слот по значению не кладётся (дважды проходит через стек
под `flip-link`): слот `&'static mut T = static` — `docs/modules/app-graph.md`.
**Узлы не падают:** у всех `restart: Never`, `TaskExit::Failed` не возвращает ни
один. Отказ связи (канал, шина, транспорт) — запись в лог и возврат в
начальное состояние автомата, а не выход и перезапуск графом; переждать отказ
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
| `cargo xtask build` | `crates-cross`: debug + release,{% if ota == "true" %} `app.bin`,{% endif %} размер образа, подпись (если включена) |
| `typos` | опечатки в коде и документации; исключения — `.typos.toml`. Нужен `typos-cli` (ставит `cargo xtask setup`), гоняется в CI отдельным джобом |
| `cargo deny check` | лицензии (только пермиссивные), источники, уязвимости зависимостей по `deny.toml`; для прошивки — с `--manifest-path crates-cross/Cargo.toml`. Нужен `cargo-deny` (ставит `cargo xtask setup`), гоняется в CI |
| `cargo xtask pins [БЛОК\|ПИН]` | справочник по чипу `{{chip}}`: выводы, альтернативные функции, DMA, тактирование |
| `cargo xtask pins --check` | не занят ли отладочный порт в `resources.rs` |

### С платой — не запускать без явного разрешения пользователя

| Команда | Что делает |
|---|---|
| `cargo xtask setup` | ставит `rustup target` и компоненты, `probe-rs`, `flip-link`, `cargo-nextest`, `cargo-deny`, `cargo-hack`, `cargo-llvm-cov`, `cargo-insta`, `typos-cli`, а для ручного запуска — `cargo-mutants`, `cargo-bloat`, `cargo-machete`, `taplo-cli` — меняет систему |
| {% if ota == "true" %}`cargo flash -p boot --release --chip {{chip}}`, затем {% endif %}`cargo run -p app` (исключение из правила выше: запускать в `crates-cross/`) | прошивка руками — необратимо; отдельной команды в `xtask` нет, и начало образа с `memory.x` здесь не сверяется — это делает `cargo xtask build` |
| `cargo xtask test target` | тесты внутри МК (`embedded-test` + `probe-rs run`); сам заливает bootloader |
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
`chip-info --check` по фильтру путей (распиновка и справочник), `release.yml`
(Release; только при `ota`) — `cargo xtask build` и подпись по тегу `v*`. Полный
перечень и что нужно для зелёного CI — README, «Что проверяет CI». Добавили шаг
в CI — допишите его сюда, компилятор такого расхождения не увидит.

Lock-файлы проект считает сам: корневой `Cargo.lock` создаёт первая
host-сборка, `crates-cross/Cargo.lock` — первая cross-сборка,
`crates-host/chip-info/Cargo.lock` — первый `cargo xtask pins`. **Их надо
закоммитить**, а не принять за мусор сборки. Шаблон их не раздаёт намеренно:
состав пакетов зависит от ответов при генерации (`ota`, `config`, `signed`,
`protocol`), и один шаблонный lock был бы верен одному сочетанию, а остальным
ронял бы `cargo fetch --locked` в CI.

Первые две команды железа не требуют вовсе, третья и четвёртая — только тулчейн с
целевым target'ом (ставится `cargo xtask setup`). Два оставшихся этапа тестирования
нужны с подключённой платой:

```
cargo xtask test target        # тесты внутри МК (embedded-test + probe-rs run)
cargo xtask test host-target   # хост управляет прошитым устройством
```

В `xtask` намеренно только то, что не делается одной командой `cargo`: две сборки
подряд, два воркспейса с разными наборами флагов и этапы тестирования (вместе с
прошивкой перед ними в правильном порядке — сначала bootloader). Обёрток над `probe-rs` (`run`, `attach`,
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
  получает от `bsp` псевдоним (`pub type X = ...`) — `docs/architecture.md`.
- Ровно один `#[panic_handler]` и один активный `#[global_logger]` на бинарник: логгер
  резолвится линковкой по имени символа и даёт `multiply defined!` при нарушении;
  паникёр один на `app` и `boot` — `crates-cross/bsp/src/panic.rs` за фичей
  `panic-handler` (у `target-tests` свой), выбирается `#[cfg(debug_assertions)]`
  (`boot` собирается только в release): dev — `use panic_probe as _;`, release — свой
  `#[panic_handler]` поверх `panic_persist::report_panic_info` (фича
  `custom-panic-handler`) — `docs/diagnostics.md`, `docs/conventions.md`.
- `prepare(len)` порта `FirmwareUpdate` зовётся один раз перед приёмом образа
  (внутри `domain::ota::download::Transfer::start`), `write()` сектор больше не
  стирает — `docs/ota.md`.
- `lib.rs` только объявляет: атрибуты, `//!`, `mod`, `pub use` — определения
  живут в модулях — `docs/conventions.md`.
- Образ приходит командами (`ports::OtaCommand`) на вход `CMD` узла OTA —
  запрос с ответом, и каждая команда получает исход (`ports::OtaReply`), который
  протокол отдаёт хосту в своём виде (protobuf — `Ok`/`Error`, Modbus и
  CANopen — код `ports::reply_code`, `docs/ota.md`). Своего канала у OTA нет: узел протокола
  только переводит сообщения в команды. Запрос несёт номер клиента —
  транспорта (`domain::ota::OtaClient`, номер из генерации): приём держит один
  владелец, чужим `Start`/`Chunk`/`Finish` — `Busy` до `Finish`, отказа или
  срока молчания. МК узел сбрасывает только по команде
  `Reboot` (порт `ports::Reboot`, `bsp::cpu::Reboot`), не по `Finish`: хост
  узнаёт исход до сброса. Отказы до стирания обязаны оставаться такими:
  `Busy` и отсутствие подписи — до `Transfer::start`, длина — внутри него, до
  `prepare` — `docs/ota.md`.
- `NoopRawMutex` у `bsp::flash::FlashMutex`, у мьютексов разделов `DFU` и `STATE`
  (`bsp::ota::DFU_FLASH`, `bsp::ota::STATE_FLASH`), `CONFIG`
  (`bsp::settings::CONFIG_FLASH`; с `ota=external` —
  `bsp::external::ExternalMutex`{% if dfu_bank == "true" %}; `DFU` во втором банке —
  `bsp::flash::DfuBankMutex`{% endif %}){% if has_modbus == "true" %}, карты регистров
  Modbus (`domain::modbus::MapMutex`, `crate::modbus` в `app`; ручки
  `SharedRegisters` — в `local`-слотах `REGS` узлов `MODBUS_*`){% endif %} и `local` на слотах узла OTA
  (`domain::ota`) — одно допущение «один исполнитель», менять вместе —
  `docs/flash.md`, `docs/architecture.md`.
- Подтверждение образа (`mark_booted`) — в стартовом действии узла OTA
  (`domain::ota`, `begin`, не удалось — ещё раз на следующем `Start`) и один
  раз за запуск (слот `CONFIRMED`). Не
  возвращать его в `main` до исполнителя: драйвер внешней флеши может ждать
  таймер. Не звать после принятого образа (`mark_updated` ставит тот же
  флаг): после обмена и до него `STATE` одинаково `Swap`, и повтор снял бы
  принятое обновление. Узел `USB` встаёт на шину только после этой попытки
  (`ready` узла OTA и `deps: [OTA ready]` у `USB_FRAG`, без графа — сигнал
  `READY_<транспорт>` в `crates-cross/app/src/tasks/ota.rs`): подтверждение после обновления или отката стирает
  `STATE`, ядро стоит на выборке, и перечисление, попавшее на это, хост
  бракует до переподключения — `docs/ota.md`.
- С `ota=external` (`DFU`/`STATE` на внешней флеши): `EXT_PAGE_SIZE` — одно
  число из генерации — переменная среды `[env]` в
  `crates-cross/.cargo/config.toml`, общая у `boot` и `bsp::external`, — и стирание
  драйвера обязано его делить (`const`-assert в обоих); драйвер —
  два слота одной микросхемы (блокирующий в `boot`, асинхронный в `bsp`).
  Bootloader загружает текущий образ без обмена, только если `STATE` не
  читается (`BootPlan::BootCurrent`) или (с подписью) новый образ отвергнут; при
  читаемом `STATE` (`BootPlan::Prepare`) — строгий `prepare`, `try_prepare` не
  используется, прыжок в наполовину переписанный `ACTIVE` недопустим — `docs/flash.md`, `docs/ota.md`.
{%- if swap_paged == "true" %}
- `SWAP_PAGE_SIZE` — число из генерации ({{page_size}}), переменная среды
  `[env]` в `crates-cross/.cargo/config.toml`; константа — `bsp::ota::SWAP_PAGE_SIZE`,
  её берёт и `boot`:
  страница обмена крупнее физической, `ACTIVE` и `DFU`
  оборачиваются в `ErasePage<_, SWAP_PAGE_SIZE>` ради малого журнала `STATE`
  (подписанному `boot` нужна зона ≥ 26 KiB). `STATE_ERASE_SIZE` — другое число
  (страница сектора `STATE`) — `docs/flash.md`.
{%- endif %}
- `STATE_ERASE_SIZE` — число из генерации, одно на `boot` и приложение (переменная
  среды в `crates-cross/.cargo/config.toml`, константа — `bsp::ota::STATE_ERASE_SIZE`). {% if state_paged == "true" %}Здесь `BOOTLOADER_STATE` строится над
  `flash_page::ErasePage` со страницей своего сектора: над цельным `Flash` на F2/F4/F7 раздел не
  кратен его странице, и `Partition::new` паникует на старте.{% else %}Здесь сектор `STATE` равен
  странице цельного `Flash`, поэтому раздел строится прямо над ним, а равенство держит
  `const`-assert рядом с константой (`bsp::ota`).{% endif %} Так же и `CONFIG`
  (`CONFIG_ERASE_SIZE` в `bsp::settings`): с OTA он может лежать в мелких секторах перед
  `ACTIVE` — `docs/flash.md`.
{%- if bank_switch != "" %}
- Флеш этого чипа с завода двухбанковый, а раскладка одобанковая:
  `bsp::banks::ensure_single_bank()` стоит первой строкой и в `boot`, и в
  `Board::new` — до любого `Flash::new`. Не переносить ниже и не убирать:
  `embassy-stm32` паникует на несовпадении режима (G0/G4) или стирает не те
  страницы (L4+). Новая плата прошивается дважды: первая прошивка
  переключает режим и перезагружает МК — `docs/flash.md`.
{%- endif %}
- `bsp::fault::enable()` (ловушка деления) — в `Board::new` сразу за
  `embassy_stm32::init`. Обработчик `HardFault` с причиной держит в образе не
  вызов, а фича `bsp/fault-handler` (её включают `app` и `target-tests`): у
  `#[exception]` экспортируемое имя, и с LTO он попадает в любой бинарник,
  линкующий `bsp`. Убрали фичу у `app` — фолт снова молчит; включили у `boot` —
  около 14 KiB зоны bootloader'а (`docs/clocks.md`, «Ядро»,
  `docs/architecture.md`).
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
  мультиплексором RNG) или прошивка виснет в ожидании слова — в `Board::new`
  или в затравке стека в `Parts::new` (F2/F4/F7, L0, L4, U0) —
  `docs/modules/bsp-rng.md`. Блок `RNG` берёт группа `rng` из
  `resources.rs`, а генератор плата отдаёт в `bsp::Drivers::rng`: им
  `app::parts::Parts::new` засевает сетевой стек и {% if net_services contains "ssh" %}отдаёт его бэкенду
  `getrandom` (`app::rng`, символ `__getrandom_v03_custom`; бэкенд выбирает
  `--cfg getrandom_backend="custom"` в `crates-cross/.cargo/config.toml`) —
  другого пути к генератору нет, SSH берёт байты через `getrandom`{% else %}дропает его, так что блок
  включён только на время затравки (бэкенд `getrandom` над ним заводит
  служба `ssh`){% endif %}.
  Блок и клок заводятся только с сетевым
  транспортом (tcp/udp), в остальных проектах `bsp/src/rng.rs` и
  `docs/modules/bsp-rng.md` хук удаляет.
{%- endif %}
{%- endif %}
{%- if protocol == "canopen" %}
- Буфер сохранённых объектов CANopen и колбэки стека над ним — локальные
  `domain::canopen::task`, автомат получает их аргументами стартового действия
  (`start(..) => begin` в таблице, `ctx.run(&persisted, &mut callbacks)`), а
  `Node<'f>` живёт в состояниях. Не переносить их в `static` или слот: тип
  замыкания там не назвать, а колбэки `zencan` своих данных не получают.
  Ключи `OBJECTS_KEY` и `NODE_ID_KEY` хранилища настроек заняты узлом{% if config == "true" %}, и хранилище
  (`crate::settings::new(parts.board.settings)`) забирает он — своим настройкам
  проекта нужен общий доступ через него{% endif %} —
  `domain::canopen`.
{%- endif %}
{%- if dfu_bank == "true" %}
- `DFU` — во втором банке, который стоит за разрывом в адресах: `boot` и
  `bsp` пишут его через `Bank2Region`, а символы `__bootloader_dfu_*` в
  `memory.x` отсчитаны от начала банка 2, а не от базы flash. Менять только
  вместе; цельный `Flash` туда не дотягивается (`FLASH_SIZE` — сумма банков) —
  `docs/flash.md`.
{%- endif %}
- Хранилище настроек — `adapters::settings` над любым async `NorFlash`. Носитель
  отдаёт плата (`bsp::settings::ConfigFlash` и его `RANGE`): внутренний флеш,
  EEPROM L0/L1 (доступ — порт `ports::EepromAccess`, обёртку
  `adapters::eeprom::Eeprom` строит `app`) или внешняя флеш (драйвер подставляет
  проект, в шаблоне — заглушка `bsp::stub::Flash`); само хранилище собирает
  `crates-cross/app/src/settings.rs`. EEPROM под хранилищем инвертирована: без
  этого заводские нули выглядели бы для `sequential-storage` закрытыми
  страницами — `docs/flash.md`.
{%- if graph == "true" %}
- У каждого узла с `watchdog:` своя связанная цепочка таймаутов (период
  отметок узла < `domain::heartbeat::WATCHDOG` и `domain::cpu_load::WATCHDOG`,
  и обе < `bsp::wdg::HW_TIMEOUT_US` минус `WATCHDOG_CHECK_EVERY` графа) —
  менять только вместе; левое звено держит `const`-assert в своём фрагменте,
  правое — `const`-assert в `crates-cross/app/src/graph.rs`, и новый узел с
  `watchdog:` обязан встать в его список — `docs/watchdog.md`.
- Сторож запускается в прологе `spawn_all` (`= board.watchdog.arm()`), а не в
  `Board::new`: с этого момента железо тикает, а кормит его только тикер графа.
  В release с OTA его раньше запускает bootloader — только перед самым прыжком в
  образ (обмен разделов и проверка подписи идут без сторожа), с
  тем же `HW_TIMEOUT_US` (число в `[env]` crates-cross/.cargo/config.toml у
  обоих общее), и путь от прыжка до
  `spawn_all` обязан в него уложиться — `docs/watchdog.md`. Без графа сторожа нет
  ни в `boot`, ни в приложении: зависший до `mark_booted` образ сам не откатится.
{%- endif %}
- Цикл исполнителя в `crates-cross/app/src/main.rs` поднят руками, а не
  `#[embassy_executor::main]`: только так измеряется сон задач, на котором стоит
  узел `CPU_LOAD`. Возврат к макросу молча ломает измерение (счётчик сна
  перестаёт расти, узел публикует 100%), и ничем, кроме линтера, это не
  ловится — `docs/modules/app-cpu-load.md`.
{%- if protocol != "none" %}
- Транспорт протокола (кадрирование, буферы, сокет) собирает `app`
  (`crates-cross/app/src/link.rs`) над тем, что отдала плата: потоком UART,
  потоком CDC, сетевым стеком. Буфер заводится там, где создаётся объект,
  который его держит: размер и порт — решение приложения, и `bsp` их не знает.
  Сборка — в выражении входа узла в `fragments:` графа или, без графа, в
  `spawn` модуля задачи (`crates-cross/app/src/tasks/`): по модулю на задачу,
  `main.rs` только зовёт `tasks::spawn_all`.
{%- endif %}
- `#![allow(dead_code)]` в начале `crates-cross/app/src/graph.rs` — не
  украшение, а условие `-D warnings`: файл целиком генерируемый, а
  `*_receiver()`/`provide_*`/`*_take()` в сыром шаблоне никто не зовёт, потому
  что подписчиков нет. Снимать allow нельзя, пока у графа нет читателей; писать
  в `graph.rs` свою логику — тоже.
- `Board` отдаёт объекты платы — периферию под трейтами (`embedded-io-async`,
  `embedded-storage`, драйверы `embassy-*-driver`, порты `ports` там, где
  стандартного трейта нет, `HardwareWatchdog`), разделы флеша, данные о чипе
  (`bsp::ChipSpec`), — и ничего не настраивает: ни периферии ядра, ни
  `Peripherals`, ни `Clocks` в полях нет. Адаптеров (кадрирование, стек, USB-классы,
  адаптер обновления, хранилище), их буферов и статиков в `bsp` нет: их собирает
  `app` (`link`, `usb`, `net`, `ota`, `settings`), буфер — там, где создаётся
  объект, который его держит. Заглушка периферии до драйвера — `bsp::stub` —
  `docs/architecture.md`.
- `Board::new()` отдаёт `(Board, Drivers)`: в `Drivers` — драйверы, над которыми
  стек отдаёт объекты сразу нескольким узлам (USB: устройство и поток CDC;
  Ethernet: фоновая работа и стек). Пару собирает один раз
  `crates-cross/app/src/parts.rs` (`Parts { board, пары }`), граф берёт `Parts`
  `boot:`-объектом и делит по полям, без графа — `tasks::spawn_all`. Не
  передавать половину пары через `static` (поток CDC не `Send`) и не делать
  поля `Board` `Option` — `docs/architecture.md`.
- `bsp` зависит только от `ports`, `flash-page` (страница раздела, общая с `boot`)
  и HAL с трейтами драйверов — не от `adapters`, `embassy-net`, `embassy-usb` и
  не от `domain`: адаптеры и типы домена — например вход узла OTA — собирает
  из полей `Parts` compose-site в `app` — `docs/architecture.md`.
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
