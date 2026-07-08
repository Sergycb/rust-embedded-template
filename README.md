# rust-embedded-template

`cargo-generate` шаблон для embedded-проектов на STM32 (Rust, `no_std`, [Embassy](https://embassy.dev)),
с трёхэтапной моделью тестирования и единой точкой входа для сборки/тестов через `xtask`.

## Генерация проекта

```sh
cargo generate --git https://github.com/Sergycb/rust-embedded-template
```

Шаблон спросит:

- `chip` — идентификатор чипа для `probe-rs` (например `STM32F407VETx`);
- `chip_feature` — фича `embassy-stm32` для этого чипа (например `stm32f407ve`);
- `target` — целевой target triple (`thumbv7em-none-eabihf` и т.п.);
- `cpu` — `target-cpu` для rustflags (`cortex-m4` и т.п.);
- `write_size` — размер страницы/блока flash для bootloader-операций (`2048` для
  STM32F1/F3/L4/G4, у F4/F7/H7 сектора крупнее — см. reference manual чипа);
- `ci` — CI-провайдер сгенерированного проекта: `github` / `gitlab` / `none`.

После генерации нужно вручную заполнить `MEMORY {}` в `cross/*/memory.x` — точные адреса и
размеры flash/RAM/bootloader-разделов зависят от конкретного чипа и от того, сколько места вы
хотите отвести под bootloader и DFU-раздел.

## Структура проекта

Два независимых Cargo workspace, потому что `cross` собирается под другой target и не может
делить `Cargo.lock`/профили с host-ориентированной частью:

```
├── domain/            # Бизнес-логика, no_std. Корневой workspace.
│   ├── adapters/       # Реализации портов
│   └── ports/          # Трейты-границы домена
├── host-target-tests/  # Тесты, гоняющие хост против прошитого устройства
├── xtask/              # Единая точка входа для сборки/тестов/флешинга
├── Cargo.toml           # Корневой workspace: domain, adapters, ports, xtask, host-target-tests
│
└── cross/               # Отдельный workspace, aппаратная часть (thumbv7em-none-eabihf и т.п.)
    ├── app/              # Основная прошивка (bin)
    ├── boot/             # Bootloader (bin, embassy-boot)
    ├── bsp/               # Board support package (lib)
    ├── target-tests/     # Тесты, исполняемые прямо на устройстве
    └── Cargo.toml
```

`domain` не входит в workspace `cross` (у него другой target), а подключается туда как обычная
path-зависимость. Версии зависимостей у `root` и `cross` объявлены раздельно и намеренно не
реэкспортируются друг через друга — `domain` не должен быть фасадом для инфраструктурных крейтов
вроде `static_cell`/`heapless`, которые `cross` использует для оркестрации задач. Синхронизацию
версий между двумя `[workspace.dependencies]` со временем берёт на себя бот, выбранный через
`ci` (Dependabot на GitHub, Renovate на GitLab — см. ниже); `post-script.rhai` дополнительно
обновляет `Cargo.lock` сразу при генерации, не дожидаясь первого PR от бота.

## Архитектура и когда что применять

**Главный принцип**: `cross` остаётся минимальным — только создание статических
hardware-объектов (буферы, DMA, периферия) и оркестрация задач (`Spawner`,
supervisor-графы, watchdog). Вся остальная логика — в `domain`, даже асинхронная и
«системная» на вид (стейтчарты, RPC, конкурентные примитивы). Подробное правило
«куда класть что» и разбор пограничных случаев — в CLAUDE.md.

Ниже — курированный набор зависимостей, уже подключённых в шаблоне, с кратким
обоснованием выбора и ссылкой на рабочий пример. Полное обоснование (сравнение с
альтернативами, тонкости API) — в doc-комментарии над каждым примером.

### `domain` — бизнес-логика

| Крейт | Для чего | Пример |
|---|---|---|
| `statig` | Иерархический async-автомат как **компонент** внутри произвольной задачи — вызывающий код сам кормит события через `handle()`. | `domain/examples/statig_blinky.rs` |
| `hsmc` | Иерархический стейтчарт как **вся задача целиком** (`run()`), с таймерами и cross-task-инъекцией событий через `Sender`. В отличие от `statig`, не компонент, а вся асинхронная задача. | `domain/examples/hsmc_connection_chart.rs` |
| `typestate` | Compile-time автомат: недопустимый переход состояния — ошибка компиляции, а не runtime-проверка (в отличие от `statig`/`hsmc`). Только host/dev-dependency — не собирается на `no_std` ARM. | `domain/examples/typestate_connection.rs` |
| `type-state-builder` | Typestate-builder: `build()` доступен только когда выставлены все обязательные поля — компилятор ловит недостающее поле, а не рантайм. Не пересекается с `typestate` (тот про переходы поведения, этот — про конструирование). | `domain/examples/type_state_builder_config.rs` |
| `mitsein` | `NonEmpty`-обёртка над `heapless::Vec` — непустота как инвариант типа, а не проверка `Option`/`is_empty()` в каждом месте. Дополняет `heapless`, не конкурирует. | `domain/examples/mitsein_nonempty_buffer.rs` |
| `futures-concurrency` | `race!`/`merge` для потоков поверх `Future` — то, чего нет в `embassy_futures::select`/`select3`/`select4` (только 2-4 ветки, без `race`/`merge` для потоков). Alloc-free. | `domain/examples/futures_concurrency_race.rs` |
| `aselect` | Альтернатива `select!`, где непроигравшие ветки гарантированно НЕ отменяются на середине (cancellation-safety) — в отличие от tokio/embassy `select!`. `no_std`, zero-alloc, реализует `Stream`. | `domain/examples/aselect_stream.rs` |
| `sync_wrapper` | Заставляет компилятор считать `!Sync`-тип `Sync`, когда эксклюзивный доступ гарантирован вручную — нужен, если `Future` должен быть `Sync`, а внутри держит не-`Sync` тип (`RefCell`) в многопоточном/multi-core сценарии. | `domain/examples/sync_wrapper_example.rs` |
| `embedded-rpc` | Межзадачный (не host↔target!) request/response с zero-copy буферами через `embassy_sync`-мьютексы/wakers. | `domain/examples/embedded_rpc_service.rs` |
| `intrusive-collections` | Intrusive-коллекция: объект сам встраивает link-поле, может состоять сразу в нескольких коллекциях без отдельного выделения узла. Не дублирует `heapless` (тот копирует значения в буфер фиксированной ёмкости). | `domain/examples/intrusive_collections_wait_list.rs` |
| `miniconf` | Runtime-конфигурация устройства: адресуемое дерево настроек, доступ к листьям по JSON-пути, без аллокатора. | `domain/examples/miniconf_settings.rs` |

### `cross` — железо и оркестрация

| Крейт | Для чего | Где документирован |
|---|---|---|
| `embassy-supervisor` | Упорядоченный старт/стоп embassy-задач по графу зависимостей (`supervisor_graph!`). | `cross/app/src/task_orchestration.rs` |
| `embassy-task-watchdog` | Мультиплексирование watchdog'ов нескольких задач в один аппаратный watchdog. | `cross/app/src/task_orchestration.rs` |
| `ector` | Actor-паттерн (message-passing) между embassy-задачами. Функционально беднее `firmware-controller` (тот ещё умеет RPC + pub-sub + периодику одним макросом), но `firmware-controller` физически не собирается на `no_std` ARM (не объявляет `#![no_std]`, падает с E0463) — проверено чистой сборкой. `ector` реально собирается и работает, проверено на STM32F3Discovery. | `cross/app/src/task_orchestration.rs` |
| `bbqueue` | Zero-copy DMA-буфер: grant/commit API, DMA пишет напрямую в backing storage. Не дублирует `embassy_sync::Pipe` (тот copy-based, для обычного межзадачного обмена без DMA). | `cross/bsp/src/buffers.rs` |

### Bootloader

`cross/boot` — минимальный `embassy-boot-stm32` bootloader: определяет активный банк
flash и безусловно прыгает в него (`unsafe { bl.load(entry) }`), как и официальный
пример `embassy-boot-stm32`. Проверки целостности образа (вектора сброса/SP) не
делает — это осознанный компромисс минимального шаблона, не забытая деталь. При
генерации спрашивается `write_size` (размер страницы/блока flash для
bootloader-операций) — по умолчанию `2048` подходит для STM32F1/F3/L4/G4, для
F4/F7/H7 (крупные сектора) нужно подставить своё значение по reference manual чипа.

## Трёхэтапное тестирование

| Этап | Где выполняется | Команда |
|---|---|---|
| host | обычный `cargo test`, без железа | `cargo xtask test host` |
| target | на устройстве, `embedded-test` harness | `cargo xtask test target` |
| host-target | хост управляет прошитым устройством | `cargo xtask test host-target` |

`cargo xtask test all` прогоняет все три этапа подряд.

## Команды `xtask`

```
cargo xtask build                  # cross: сборка app+boot в debug и release
cargo xtask run [debug|release]    # прошить boot, запустить app через probe-rs run
cargo xtask flash [debug|release]  # прошить boot+app без подключения дебаггера
cargo xtask lint [cross]           # fmt --check + clippy -D warnings (host или cross)
cargo xtask deny                   # cargo-deny check (лицензии/уязвимости/дубли версий)
cargo xtask test [all|host|host-target|target]
```

## debug vs release: defmt vs log

`cross/app` и `cross/boot` — взаимоисключающие Cargo-фичи `debug` (defmt + RTT + `panic-probe`) и
`release` (`log` + `panic-halt`); при отсутствии обеих или при обеих сразу сборка не пройдёт —
это специально проверяется `compile_error!` в начале `main.rs`. `domain` и `bsp` не выбирают
профиль сами, а только прокидывают одноимённые фичи (`defmt`/`log`) дальше по зависимостям.

## Обновление зависимостей

`ci` определяет не только CI-workflow, но и бота для PR с обновлениями версий:
`github` → `.github/dependabot.yml`, `gitlab` → `renovate.json`, `none` → ни того, ни другого.
Оба покрывают обе `[workspace.dependencies]` (корневую и `cross/`).

## Инструменты

Версия Rust и компоненты (`clippy`, `rustfmt`) зафиксированы в `rust-toolchain.toml` (корень —
host-тулчейн, `cross/rust-toolchain.toml` — тот же тулчейн + целевой `target`, `rustup` поставит
его автоматически). Для прошивки/отладки нужны `probe-rs` и `flip-link`:

```sh
cargo install probe-rs-tools flip-link
```

В `.vscode/` — задачи, привязанные к `cargo xtask` (`tasks.json`), запуск отладки через
`probe-rs` (`launch.json`, `F5`) и список рекомендуемых расширений (`extensions.json`).
