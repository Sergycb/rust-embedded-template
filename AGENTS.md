# AGENTS.md

Embedded-проект на STM32 (Rust, `no_std`, [Embassy](https://embassy.dev)), собранный из
шаблона [rust-embedded-template](https://github.com/Sergycb/rust-embedded-template).
Этот файл — для ИИ-сессий, работающих с репозиторием: что здесь нельзя нарушать и почему.
Человеко-читаемое описание архитектуры и таблицы зависимостей — в README.md, разделе
«Архитектура и когда что применять». Обоснование выбора каждой конкретной библиотеки — в
doc-комментариях (`//!`) над её примером использования, см. ссылки в README — не дублируется здесь.

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
`embassy-executor`/`embassy-stm32`, тестируется на host (`cargo xtask test host`).
`crates-cross/` — аппаратная прошивка (`bsp`, `app`, `boot`), собирается под `{{target}}`,
подключает `domain` как обычную path-зависимость.
`cross` остаётся минимальным: только создание `static` hardware-объектов (буферы, DMA,
периферия) и оркестрация задач (`Spawner`, supervisor-графы, watchdog).
Всё остальное — даже асинхронное и «системное» на вид (стейтчарты, RPC, синхронизация
задач) — живёт в `domain`, а не в `cross`.
Подробности, прецеденты и пограничные случаи (например, `watchdog`) — `docs/architecture.md`.

## Команды

Рабочий каталог для всех команд ниже — корень сгенерированного проекта; `cargo xtask`
сам передаёт нужные манифесты, заходить в `crates-cross/` не надо.

### Без платы

| Команда | Что делает |
|---|---|
| `cargo xtask lint` | `domain`: `fmt --check` + `clippy -D warnings` |
| `cargo xtask test host` | `domain`: `cargo nextest` |
| `cargo xtask lint cross` | `crates-cross`: `fmt --check` + `clippy -D warnings` |
| `cargo xtask build` | `crates-cross`: debug + release,{% if ota == "true" %} `app.bin`,{% endif %} размер образа, подпись (если включена) |
| `cargo xtask precommit` | четыре команды выше подряд, до первой упавшей |
| `cargo xtask pins [БЛОК\|ПИН]` | справочник по чипу `{{chip}}`: выводы, альтернативные функции, DMA, тактирование |
| `cargo xtask pins --check` | не занят ли отладочный порт в `resources.rs` |

### С платой — не запускать без явного разрешения пользователя

| Команда | Что делает |
|---|---|
| `cargo xtask setup` | ставит `rustup target`, `probe-rs`, `flip-link`, `cargo-nextest` — меняет систему |
| `cargo xtask flash [debug\|release]` | прошивает{% if ota == "true" %} bootloader и{% endif %} приложение — необратимо |
| `cargo xtask test target` | тесты внутри МК (`embedded-test` + `probe-rs run`) |
| `cargo xtask test host-target` | хост управляет прошитым устройством |
| `cargo xtask test all` | обе строки выше |
| `cargo xtask panic` | читает регион `PANIC` с живой платы через `probe-rs` |
| `probe-rs attach --chip {{chip}} crates-cross/target/{{target}}/debug/app` | лог живой платы |
| `probe-rs erase --chip {{chip}} --connect-under-reset` | снять плату, зациклившуюся на панике |

### Порядок проверки перед коммитом

```
cargo xtask precommit     # все четыре ниже подряд, до первой упавшей
```

Разворачивается она ровно в это, и знать состав надо: когда команда упадёт,
дальше вы будете гонять один этап, а не все четыре.

```
cargo xtask lint          # domain: fmt --check + clippy -D warnings
cargo xtask test host     # domain: cargo nextest

cargo xtask lint cross    # cross: fmt --check + clippy -D warnings
cargo xtask build         # cross: debug + release
```

**`precommit` — локальное зеркало CI, а не то, что CI исполняет**, и подменять
им джобы нельзя: GitHub и GitLab разносят host и cross по разным джобам,
которые идут параллельно и с раздельными кешами. Отсюда и имя — `ci` обещало
бы связь, которой нет. Обратная сторона у зеркала одна: добавили шаг в CI —
добавьте и сюда, компилятор такого расхождения не увидит.

Первая cross-сборка в свежесгенерированном проекте создаёт
`crates-cross/Cargo.lock` — **его надо закоммитить**, а не принять за мусор
сборки. Шаблон этот файл не раздаёт намеренно: состав пакетов зависит от
ответов при генерации (`ota`, `config`, `signed`), и один закоммиченный lock
был бы верен ровно одному сочетанию из шести, а остальным ронял бы
`cargo fetch --locked` в CI. Два других lock-файла (корневой и
`crates-host/chip-info/`) приезжают из шаблона как обычно.

Первые две команды железа не требуют вовсе, третья и четвёртая — только тулчейн с
целевым target'ом (ставится `cargo xtask setup`). Два оставшихся этапа тестирования
нужны с подключённой платой:

```
cargo xtask test target        # тесты внутри МК (embedded-test + probe-rs run)
cargo xtask test host-target   # хост управляет прошитым устройством
```

В `xtask` намеренно только то, что не делается одной командой `cargo`: две сборки
подряд, два воркспейса с разными наборами флагов, прошивка в правильном порядке
(сначала bootloader) и этапы тестирования. Обёрток над `probe-rs` (`run`, `attach`,
`size`, `reset`, `erase`) здесь нет и не надо: они ничего не добавляли к прямому
вызову, а `probe-rs` в проекте всё равно обязателен.

`cargo xtask panic` этому правилу не противоречит, хотя внутри и зовёт `probe-rs
read`: руками пришлось бы сначала найти адрес региона `PANIC` в `crates-cross/app/memory.x`,
потом знать формат `panic-persist` (магия `0x0FACADE0`, длина, байты) и собирать
строку из hex-вывода. Команда делает ровно это — не удаляйте её как «ещё одну
обёртку». С появлением дампа аппаратного отказа у неё добавилась вторая работа,
которой у `probe-rs` нет в принципе: расшифровка битов `cfsr`/`hfsr` (см.
«Диагностика: дамп аппаратного отказа»). Держать таблицу расшифровки на
устройстве нельзя — это настоящий flash; значит разбирать сырые значения обязан
хост, и делает это она. То же и с `cargo xtask pins`: путь к манифесту `chip-info` относительный,
прямой вызов работал бы только из корня проекта. Смотреть лог живой платы —
`probe-rs attach --chip <CHIP> <elf>`, вытащить зациклившуюся на панике —
`probe-rs erase --chip <CHIP> --connect-under-reset`.

## Запреты

- Не прошивать плату (`flash`, `test target`, `test host-target`, `panic`, `probe-rs`)
  без явного разрешения пользователя — необратимо и требует железа.
- Не править `memory.x` и линкер-скрипты (`.cargo/config.toml`, `link.x`) — раскладка
  памяти считается генерацией шаблона, вручную не трогается.
- Не добавлять зависимости без разрешения пользователя — правила см. `docs/conventions.md`.
- Не пинить тулчейн — `rust-toolchain.toml` в шаблоне намеренно нет, см. `docs/conventions.md`.

## Инварианты, которые не ловит компилятор

- Задачи `embassy` не могут быть generic: тип, уезжающий в `#[embassy_executor::task]`,
  получает от `bsp` псевдоним (`pub type X = ...`) — `docs/architecture.md`.
- Ровно один `#[panic_handler]` и один активный `#[global_logger]` на бинарник: логгер
  резолвится линковкой по имени символа и даёт `multiply defined!` при нарушении —
  `docs/conventions.md`.
- `prepare(len)` в `domain::download::Download` зовётся один раз перед приёмом образа,
  `write()` сектор больше не стирает — `docs/ota.md`.
{%- if graph == "true" %}
- Три таймаута сторожа связаны цепочкой (`BACKOFF_MAX` < `APP_WATCHDOG` < `HW_TIMEOUT`) —
  менять только вместе — `docs/watchdog.md`.
{%- endif %}
- `Board::core` (`cortex_m::Peripherals`) забирается первой строкой `Board::init()`, до
  `init_peripherals()`: часть семейств зовёт `steal()` внутри своей инициализации —
  `docs/architecture.md`.
- Liquid в `ports`/`adapters` запрещён: оба крейта — члены корневого workspace и
  компилируются в самом репозитории шаблона — `docs/architecture.md`.
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

Не сообщать о завершении задачи, пока все четыре не прошли (`EXIT=0`) — по одной, а не
`precommit` целиком: так видно, какая именно упала.
