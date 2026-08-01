# rust-embedded-template

`cargo-generate` шаблон для embedded-проектов на STM32 (Rust, `no_std`, [Embassy](https://embassy.dev)),
с трёхэтапной моделью тестирования и единой точкой входа для сборки/тестов через `xtask`.

## Генерация проекта

```sh
cargo generate --git https://github.com/Sergycb/rust-embedded-template
```

Сначала — каскадный выбор конкретного STM32: семейство (`F`, `G`, `H`, `L`...), затем
линейка (`F0`, `F1`, `F3`, `G4`...), и дальше посимвольно, пока список не сузится до одного
чипа. Список вариантов на каждом шаге — реальные чипы, поддерживаемые одновременно и
`embassy-stm32`, и `probe-rs` (список зашит в шаблон заранее, см. ниже). По результату
автоматически подставляются:

- `chip` — идентификатор чипа для `probe-rs` (например `STM32F407VE`);
- `chip_feature` — фича `embassy-stm32` для этого чипа (например `stm32f407ve`);
- `target` — целевой target triple (`thumbv7em-none-eabihf` и т.п.);
- `cpu` — `target-cpu` для rustflags (`cortex-m4` и т.п.);
- `write_size` — параметр буфера подкачки для bootloader (`BootLoader::prepare::<_,_,_,N>`);
- фича банковой схемы `embassy-stm32` (`single-bank`) — только для чипов, у которых
  их несколько; без неё build-скрипт `embassy-stm32` просто паникует;
- `MEMORY {}` в `cross/*/memory.x` — адреса и размеры `FLASH`/`BOOTLOADER_STATE`/
  `ACTIVE`/`DFU`/`RAM`/`PERSIST`, посчитанные по реальным границам секторов вашего чипа
  (источник — `stm32-metapac`, та же зависимость, что уже тянет `embassy-stm32`), плюс
  **все остальные регионы памяти чипа** под своими именами: `ITCM`, `AXISRAM`, `CCMRAM`,
  `BKPSRAM`, `EEPROM`, `OTP` и т.д. Окна внешних шин (`FMC_*`, `SDRAM_*`, `OCTOSPI_*`) и
  вторые окна того же блока (`CCMRAM_ICODE` при `CCMRAM_DCODE`) выводятся
  закомментированными с причиной: за первыми нет памяти, пока микросхема не распаяна и
  не настроен контроллер, а вторые — та же физическая память по другому адресу, и
  разместить данные в обоих окнах разом линкер не помешает;
- реализация инициализации HAL под ваш класс чипа: одноядерный получает
  `embassy_stm32::init()`, двухъядерный — `init_primary()` с `SharedData`. В проект
  попадает только одна ветка, без `#[cfg]` и без кода для чужого класса чипа;
  Файлы у `cross/app` и `cross/boot` **разные**: приложение линкуется в `ACTIVE`
  (с базы flash стартует bootloader), bootloader — в свою зону до `BOOTLOADER_STATE`,
  чтобы разросшийся образ ловил линкер, а не молчаливое наложение на `ACTIVE`.

Чтобы задать чип без интерактивного каскада (например в CI/скриптах), передайте
`--define chip_feature=stm32f407ve` — остальные поля выведутся из него автоматически
(опечатка в значении будет замечена сразу, до генерации файлов). Любое из `chip`/`cpu`/
`target`/`write_size` можно и переопределить по отдельности своим `--define` — например
если дефолтный `chip` указывает не на самый специфичный вариант probe-rs для вашего чипа.
`--define write_size=...` заодно отключает автогенерацию `memory.x` (адреса привязаны
именно к вычисленному `write_size`) — в этом случае `MEMORY {}` в обоих `cross/*/memory.x`
придётся заполнить вручную, как раньше.

### Чипы, куда OTA не помещается

`memory.x` заполняется автоматически для всех чипов каскада, но схема с bootloader'ом
влезает не в каждый. Ей нужно минимум пять стираемых страниц: `BOOTLOADER` + отдельная
страница `BOOTLOADER_STATE` (её стирают независимо от кода) + `ACTIVE` + `DFU`, который
по требованию алгоритма swap на страницу больше `ACTIVE`. На чипе с крупными секторами
это много: у STM32H723VE, например, 512 KiB flash одним регионом с сектором 128 KiB —
всего четыре страницы, и валидной раскладки не существует ни при каком дележе.

Для таких чипов генерируется проект **без OTA**: один образ на весь flash, каталог
`cross/boot` в проект не попадает, `cargo xtask flash` прошивает только приложение.
Причина печатается при генерации и остаётся комментарием в самом `memory.x`:

```
OTA отключён для h723ve: flash 512 KiB при секторе 128 KiB: под BOOTLOADER+STATE
уходит 256 KiB, на ACTIVE+DFU остаётся стираемых страниц: 2 (нужно 3)
```

Если OTA нужен именно на этом чипе — вариантов два: взять модель с большим flash (тот же
корпус, например STM32H723VG на 1 MiB — у него раскладка считается) или вынести `DFU` на
внешнюю flash (`embassy-boot` умеет работать с любым `NorFlash`; шаблон этого не делает —
периферия и разводка зависят от платы).

**Двухъядерные чипы** (STM32H745/747/755/757, STM32WL54/55 — выбираются в каскаде явно,
отдельным вариантом на каждое ядро) собираются (проверено компиляцией, не на железе), но
код пишется только под ОДНО, выбранное в каскаде ядро (например `stm32h745zi-cm7` — CM7):
шаблон не генерирует для второго ядра ни кода, ни отдельной прошивки. **На STM32H7 (не
WL) это НЕ значит, что второе ядро бездействует** — по умолчанию оно тоже стартует при
сбросе и без явной проверки/снятия option byte `BCM4` перед прошивкой может исполнять
чужой код поверх общей периферии; подробности и что проверить — в CLAUDE.md, раздел про
dual-core. Полноценная symmetric/asymmetric AMP-поддержка (два независимых образа,
синхронизация между ними) — осознанно не входит в шаблон, это отдельная архитектурная
задача.

Дополнительно шаблон спросит `ci` — CI-провайдер сгенерированного проекта:
`github` / `gitlab` / `none`.

Список чипов для каскада вшит в `chip-select.rhai` заранее (мейнтейнером шаблона, не
опрашивается заново при вашей генерации) и перегенерируется командой
`cargo run --manifest-path chip-data-gen/Cargo.toml` при обновлении версии `embassy-stm32`
в `cross/Cargo.toml` или локально установленной у мейнтейнера версии `probe-rs-tools`.
`chip-data-gen` — инструмент обслуживания самого шаблона, в сгенерированные проекты не
попадает.

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

Часть из них — `fsm`, `fsm-async`, `sync-request`, `typestate`, `supervisor`,
`watchdog` — берётся из [rust-lib](https://github.com/Sergycb/rust-lib) как
git-зависимость; они заменили сторонние аналоги (`statig`, `hsmc`,
`embedded-rpc`, `typestate` 0.9.0-rc2, `embassy-supervisor`,
`embassy-task-watchdog`, `ector`). Чем именно каждый из них лучше того, что
стоял раньше, написано в примере/doc-комментарии, на который ссылается таблица.

### `domain` — бизнес-логика

| Крейт | Для чего | Пример |
|---|---|---|
| `fsm` | Иерархический автомат как **компонент** внутри произвольной задачи: владелец сам подаёт события через `dispatch()`. Синхронный, без исполнителя и таймеров. | `domain/examples/fsm_light_machine.rs` |
| `fsm-async` | Тот же `fsm::Machine`, но владеющий **своей задачей целиком**: `run()` сам ждёт события из `embassy_sync`-канала и взводит таймауты состояний по `embassy_time`. Не другая модель состояний, а другой владелец цикла. | `domain/examples/fsm_async_connection_chart.rs` |
| `typestate` | Compile-time автомат: недопустимый переход состояния — ошибка компиляции, а не runtime-проверка (в отличие от `fsm`/`fsm-async`). Настоящий `no_std`, поэтому обычная зависимость, а не только host/dev. | `domain/examples/typestate_connection.rs` |
| `type-state-builder` | Typestate-builder: `build()` доступен только когда выставлены все обязательные поля — компилятор ловит недостающее поле, а не рантайм. Не пересекается с `typestate` (тот про переходы поведения, этот — про конструирование). | `domain/examples/type_state_builder_config.rs` |
| `mitsein` | `NonEmpty`-обёртка над `heapless::Vec` — непустота как инвариант типа, а не проверка `Option`/`is_empty()` в каждом месте. Дополняет `heapless`, не конкурирует. | `domain/examples/mitsein_nonempty_buffer.rs` |
| `futures-concurrency` | `race!`/`merge` для потоков поверх `Future` — то, чего нет в `embassy_futures::select`/`select3`/`select4` (только 2-4 ветки, без `race`/`merge` для потоков). Alloc-free. | `domain/examples/futures_concurrency_race.rs` |
| `aselect` | Альтернатива `select!`, где непроигравшие ветки гарантированно НЕ отменяются на середине (cancellation-safety) — в отличие от tokio/embassy `select!`. `no_std`, zero-alloc, реализует `Stream`. | `domain/examples/aselect_stream.rs` |
| `sync_wrapper` | Заставляет компилятор считать `!Sync`-тип `Sync`, когда эксклюзивный доступ гарантирован вручную — нужен, если `Future` должен быть `Sync`, а внутри держит не-`Sync` тип (`RefCell`) в многопоточном/multi-core сценарии. | `domain/examples/sync_wrapper_example.rs` |
| `sync-request` | Межзадачный (не host↔target!) request/response поверх `embassy_sync`, без зависимости от `embassy-executor`. Отменённый (по таймауту, проигравшей веткой `select!`) запрос помечен номером поколения и не «протечёт» ответом в следующий. | `domain/examples/sync_request_service.rs` |
| `miniconf` | Runtime-конфигурация устройства: адресуемое дерево настроек, доступ к листьям по JSON-пути, без аллокатора. | `domain/examples/miniconf_settings.rs` |
| `test-log` | Автоинициализация `log` в host-тестах (тихо на успехе, видно на провале/`--nocapture`), цветной вывод из коробки — без ручного `env_logger::init()` в каждом тесте. | `domain/tests/logging.rs` |

### `cross` — железо и оркестрация

| Крейт | Для чего | Где документирован |
|---|---|---|
| `supervisor` | Весь граф embassy-задач одним макросом `supervisor_graph!`: упорядоченный старт по `deps:`, рестарты с backoff, graceful shutdown, передача периферии через `resources:` (переживает перезапуск задачи), почтовые ящики `inbox:`, broadcast `publish:`/`subscribe:`, RPC `request:`/`calls:` и общий watchdog. Заменил связку `embassy-supervisor` + `embassy-task-watchdog` + `ector`, где один и тот же граф описывался тремя независимыми DSL. | `cross/app/src/task_orchestration.rs` |
| `watchdog` | `no_std`-ядро мультиплексора: несколько программных watchdog'ов задач, каждый со своим таймаутом, поверх одного аппаратного. Привязку к железу задают два трейта (`Clock`, `HardwareWatchdog`), реализуемые под конкретный МК; `supervisor` использует его из блока `watchdog:`. | `cross/app/src/task_orchestration.rs` |
| `bbqueue` | Zero-copy DMA-буфер: grant/commit API, DMA пишет напрямую в backing storage. Не дублирует `embassy_sync::Pipe` (тот copy-based, для обычного межзадачного обмена без DMA). | `cross/bsp/src/buffers.rs` |
| свой `#[global_logger]` над `bbqueue` | UART/любой другой транспорт для `release` без пробника — turnkey-крейта нет: `defmt-bbq` и `defmt-serial` держат `defmt@0.3.x` при `defmt@1.1.0` у нас (`symbol multiply defined` при реальной проверке), а `defmt-embassy-usbserial` требует `embassy-usb ^0.5` вместо версии, парной к текущей `embassy-stm32`. | `cross/app/src/task_orchestration.rs` |

### Bootloader

`cross/boot` — минимальный `embassy-boot-stm32` bootloader: определяет активный банк
flash и безусловно прыгает в него (`unsafe { bl.load(entry) }`), как и официальный
пример `embassy-boot-stm32`. Проверки целостности образа (вектора сброса/SP) не
делает — это осознанный компромисс минимального шаблона, не забытая деталь. `write_size`
(параметр буфера подкачки `BootLoader::prepare::<_,_,_,N>` — не путать ни с минимальной
порцией записи flash, у STM32 она всегда маленькая, 4-8 байт, ни с `PAGE_SIZE`, который
`embassy-boot` вычисляет сам из `NorFlash::ERASE_SIZE`) и оба `memory.x` вычисляются
автоматически по выбранному чипу — см. «Генерация проекта» выше. На чипах, куда схема не
помещается, `cross/boot` в проект не входит вовсе — см. там же.

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
cargo xtask test [all|host|host-target|target]
```

## debug vs release: defmt и паникёр — одни и те же

`cross/app` и `cross/boot` используют `defmt` в обоих профилях (`log` в прошивке не
используется вовсе, только в host-тестах `domain` — см. ниже). Паникёр — `panic-probe`
тоже в обоих: он печатает бэктрейс через defmt, когда пробник подключён, и упирается в
`udf`, когда нет (RTT без читателя просто отбрасывает вывод, а не блокируется).
Наблюдаемый результат тот же, ради которого в release раньше отдельно подключался
`panic-halt` — устройство встаёт, — но путь другой: не `loop {}` прямо в
`#[panic_handler]`, а HardFault, который гасится дефолтным хендлером `cortex-m-rt`.
Разница видна, если завести свой HardFault-хендлер (например, пишущий причину в
PERSIST-регион перед сбросом): паника пойдёт через него. Ни Cargo-фич `debug`/`release`,
ни `#[cfg(debug_assertions)]`-развилок для выбора паникёра больше нет.

`defmt-rtt` — дефолтный транспорт в обоих профилях; паттерн для `release` без пробника
(UART, когда на плате появится нужная периферия) задокументирован в
`cross/app/src/task_orchestration.rs`. При его подключении держите инвариант «ровно один
активный `#[global_logger]` на бинарник» — там же объяснено, почему линкер на этом
падает жёстко. Подробности — в CLAUDE.md.

## Обновление зависимостей

`ci` определяет не только CI-workflow, но и бота для PR с обновлениями версий:
`github` → `.github/dependabot.yml`, `gitlab` → `renovate.json` плюс джоб `renovate`
в `.gitlab-ci.yml`, `none` → ни того, ни другого. Оба покрывают обе
`[workspace.dependencies]` (корневую и `cross/`).

Разница между ними важна на практике: **dependabot работает сразу после пуша** — его
исполняет сама платформа, ничего настраивать не нужно. **Renovate так не умеет**:
`renovate.json` — это конфигурация репозитория, а не запускалка, и без того, кто его
выполняет, никаких MR не появится. Поэтому в `.gitlab-ci.yml` есть отдельный джоб
`renovate` (образ `renovate/renovate`, запускается ТОЛЬКО по расписанию — остальные джобы
из расписания, наоборот, исключены). Чтобы он заработал, нужны две вещи со стороны
проекта:

1. **CI/CD variable `RENOVATE_TOKEN`** — Project или Group Access Token с ролью
   `Developer` и скоупом `api`. Пометьте masked; protected ставить не надо, если
   расписание будет ходить по незащищённой ветке.
2. **Pipeline schedule** (*CI/CD → Schedules*), например ежедневно ночью. Без расписания
   джоб не запустится никогда — на обычных push/MR он намеренно не срабатывает.

Альтернатива джобу — подключить к проекту [Mend Renovate App](https://docs.renovatebot.com/getting-started/running/)
(он поддерживает и GitLab); тогда джоб можно удалить.

## Инструменты

Тулчейн не запинен (`rust-toolchain.toml` в шаблоне нет) — ставьте `clippy`/`rustfmt` и
целевой target сами (`rustup component add clippy rustfmt`, `rustup target add <target>`),
как это делает CI. Минимальная версия Rust — 1.96 (её требуют крейты из `rust-lib`).
Для прошивки/отладки нужны `probe-rs` и `flip-link`:

```sh
cargo install probe-rs-tools flip-link
```

В `.vscode/` — задачи, привязанные к `cargo xtask` (`tasks.json`), запуск отладки через
`probe-rs` (`launch.json`, `F5`) и список рекомендуемых расширений (`extensions.json`).
