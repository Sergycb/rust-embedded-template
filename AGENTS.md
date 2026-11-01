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
входа и паникёр у него свои) и от `domain` ради одной свободной функции над
блокирующими разделами, `domain::ota::boot::screen_update`; в образ попадает
только она (`lto` + `--gc-sections` выбрасывают остальное). Типы — `Screening`,
`ScreenFailure` — тоже в `domain::ota::boot`. Больше `cross` в `domain` ходит только
через `app`; `bsp` от `domain` не зависит — `docs/architecture.md`.
{%- else %}
`cross` в `domain` ходит только через `app`; `bsp` от `domain` не зависит —
`docs/architecture.md`.
{%- endif %}
`cross` остаётся минимальным: `bsp` — рычаги платы (объекты под трейтами и
портами; DMA-буферы под железо), `app` — оркестрация задач (`Spawner`{% if graph == "true" %}, supervisor-графы, watchdog{% endif %}) и
память, которую домен получает ссылками (буферы узлов и транспортов{% if has_modbus == "true" %}, карта
Modbus{% endif %}{% if net_services != "" %}, секреты служб{% endif %}).
**Реализация над стандартным трейтом — `adapters`, над чипом — `bsp`**: HAL-объект
с трейтом `bsp` отдаёт псевдонимом, своя структура в `bsp` — только обёртка там,
где реализации трейта нет. **Кто строит адаптер, тот держит его память:**
адаптер, без которого рычаг не собрать, — плата в `Board::new`; адаптер, чей
тип или память выбирает приложение, — compose-site ({% if graph == "true" %}привязка узла в
`fragments:` графа, память — `static [u8; N]` там же{% else %}модуль задачи, память — его статики{% endif %}), и узел получает его
готовым во входе. Оба правила с пограничными случаями и порт против библиотеки
прямо в `domain` — `docs/architecture.md`, «Адаптеры делятся по одному признаку — нужен
ли чип» и «Порт и адаптер — или библиотека прямо в `domain`».
Всё остальное — даже асинхронное и «системное» на вид (стейтчарты, RPC, синхронизация
задач) — живёт в `domain`, а не в `cross`, и узлы тоже (всё — `docs/architecture.md`):
- подсистема объявляет узел `supervisor_fragment!`-ом рядом со своей задачей —
  самодостаточным модулем, все имена внутри полным путём (`$crate::…`), типы,
  которых он не видит, — параметрами{% if graph == "true" %}; `crates-cross/app/src/graph.rs` собственных
  узлов не объявляет — только `boot:`, блок `watchdog:` и `fragments:` с именами
  узлов{% endif %} — «Узлы — фрагменты в `domain`»;
- **каждая задача узла — автомат `#[fsm::typestate]`** на контексте фрагмента,
  задача — `match ctx.run().await {}`; голый `loop` в функции `task` и проекция
  аргументов (`task: f(ctx.поле)`) не пишутся{% if graph != "true" %} (исключение — `cpu_load::log_load`,
  не узел){% endif %} — «Каждая задача узла — автомат `#[fsm::typestate]`»;
{%- if graph == "true" %}
- объект от килобайта в слот по значению не кладётся: слот
  `&'static mut T = static` — `docs/modules/app-graph.md`;
{%- endif %}
- **узлы не падают:** `restart:` нет, задача возвращает `()`, отказ связи —
  лог и возврат в начальное состояние автомата или переход в себя; переждать
  отказ — контракт порта — «Узлы не падают»;
- цена — зависимость `domain` от `supervisor` (транзитивно и от
  `embassy-executor`); `embassy-stm32` в `domain` нет — «Цена решения…».

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
{%- if ota == "true" %}
| `cargo xtask sign` | подписать уже собранный `app.bin` ключом `ota-signing-key.bin`; так подписывает релиз — ключ появляется после сборки |
{%- endif %}
| `typos` | опечатки в коде и документации; исключения — `.typos.toml`. Нужен `typos-cli` (ставит `cargo xtask setup`), гоняется в CI отдельным джобом |
| `cargo deny check` | лицензии (только пермиссивные{% if has_canopen == "true" %}; исключение — MPL-2.0 у `zencan`{% endif %}), источники, уязвимости зависимостей по `deny.toml`; для прошивки — с `--manifest-path crates-cross/Cargo.toml`. Нужен `cargo-deny` (ставит `cargo xtask setup`), гоняется в CI |
| `cargo xtask pins [БЛОК\|ПИН]` | справочник по чипу `{{chip}}`: выводы, альтернативные функции, DMA, тактирование |
| `cargo xtask pins --check` | распиновка во всём `crates-cross/bsp/src`: занятый отладочный порт — ошибка, выводы кварца и MCO — предупреждение |

### С платой — не запускать без явного разрешения пользователя

| Команда | Что делает |
|---|---|
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

{% if ci != "none" %}Это основа CI — там host и cross разнесены по разным джобам, которые идут
параллельно и с раздельными кешами. Сверх этих четырёх CI гоняет `cargo deny
check`, `cargo hack --feature-powerset`, покрытие (`cargo llvm-cov`) и `typos`:
все четыре ставит `cargo xtask setup`, и при правке зависимостей или фич первые
три стоит прогнать и локально.{% else %}CI в проекте нет, поэтому эти четыре гоняются руками. Сверх них —
`cargo deny check`, `cargo hack --feature-powerset`, покрытие (`cargo llvm-cov`)
и `typos`: все четыре ставит `cargo xtask setup`, и при правке зависимостей или
фич первые три стоит прогнать.{% endif %}{% if ci == "github" %} Отдельно от `ci.yml`: `pins.yml` (Pins) —
`chip-info --check` по фильтру путей (распиновка и справочник), `audit.yml` (Audit) —
`cargo deny check advisories` по обоим lock-файлам каждую ночь{% if ota == "true" %}, `release.yml`
(Release) — `cargo xtask build` и подпись по тегу `v*`{% endif %}.{% elsif ci == "gitlab" %} В `.gitlab-ci.yml` отдельно:
`pins:check` — `chip-info --check` по фильтру путей (распиновка и справочник),
`audit` — `cargo deny check advisories` по обоим lock-файлам по расписанию,
`renovate` — обновления зависимостей по тому же расписанию{% if ota == "true" %},
`release:build`/`release:publish` — `cargo xtask build` и подпись по тегу `v*`{% endif %}.{% endif %}{% if ci != "none" %} Полный
перечень и что нужно для зелёного CI — README, «Что проверяет CI». Добавили шаг
в CI — допишите его сюда, компилятор такого расхождения не увидит.{% endif %}

Lock-файлы (корневой, `crates-cross/`, `crates-host/chip-info/`) проект считает
сам первой сборкой. **Их надо закоммитить**, а не принять за мусор сборки —
README, «Быстрый старт».

Первые две команды железа не требуют вовсе, третья и четвёртая — только тулчейн с
целевым target'ом и `flip-link` (линкер прошивки; оба ставит `cargo xtask setup`).
Два оставшихся этапа тестирования нужны с подключённой платой:

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
`probe-rs attach --chip {{chip}} <elf>`, вытащить зациклившуюся на панике —
`probe-rs erase --chip {{chip}} --connect-under-reset`.

## Запреты

- Не прошивать плату (`cargo flash`, `cargo run` в `crates-cross`, `test target`,
  `test host-target`, `panic-info`, `probe-rs`) без явного разрешения
  пользователя — необратимо и требует железа.
- Не запускать `cargo xtask setup` и `cargo install` без разрешения пользователя —
  меняет систему (что ставит `setup` — README, «Инструменты»).
- Не добавлять зависимости без разрешения пользователя — правила см. `docs/conventions.md`.
- Не менять версию в `rust-toolchain.toml` без разрешения пользователя{% if ci == "gitlab" %} и без
  образов `rust:` в `.gitlab-ci.yml`{% endif %} — от неё зависит воспроизводимость
  релиза; версию `flip-link` — тоже только везде разом — `docs/conventions.md`,
  «Тулчейн запинен».

## Инварианты, которые не ловит компилятор

- Задачи `embassy` не могут быть generic: тип, уезжающий в `#[embassy_executor::task]`,
  получает псевдоним (`pub type X = ...`): рычаг — от `bsp`{% if ota == "true" or config == "true" %} ({% if ota == "true" %}`bsp::FirmwareFlash`{% if config == "true" %},
  {% endif %}{% endif %}{% if config == "true" %}`bsp::ConfigFlash`{% endif %}){% endif %}, адаптер — от `app`{% if protocol_link == "true" %} (`crate::link::<вид>[::<транспорт>]::Link`){% endif %} или
  конкретным типом прямо {% if graph == "true" %}в графе{% else %}в задаче{% endif %}{% if ota == "true" %} (`adapters::ota::Updater<bsp::FirmwareFlash>`){% endif %}
  — `docs/architecture.md`.
- Ровно один `#[panic_handler]` и один активный `#[global_logger]` на бинарник;
  паникёр у каждого бинарника свой, у `app` профиль выбирает
  `#[cfg(debug_assertions)]` в `crates-cross/app/src/panic.rs`, а не Cargo-фича —
  `docs/diagnostics.md`, «Два паникёра, один на сборку».
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
- Хвост образа — `[образ][раскладка u32 LE][версия u32 LE]` (`cargo xtask build`);
  отпечаток раскладки узел OTA сверяет на `Finish` до `mark_updated`, и сменилась
  раскладка `memory.x` — `boot` перешивается по проводу, а не образом —
  `docs/ota.md`, «Подпись OTA-образа».
{%- endif %}
{%- if shared_flash == "true" or ota == "true" or config_store == "flash" or external_flash == "true" %}
- `NoopRawMutex` у мьютексов флеша{% if ota == "true" and graph == "true" %} и `local` на слотах узла OTA{% endif %} — одно допущение
  «один исполнитель», менять вместе — `docs/flash.md`.{% if has_modbus == "true" %} Карта регистров Modbus
  (`domain::modbus::MapMutex`, `CriticalSectionRawMutex`) в него не входит —
  `docs/modules/app.md`, «Modbus: одна карта на все узлы `MODBUS_*`».{% endif %}
{%- endif %}
{%- if ota == "true" %}
- Подтверждение образа (`mark_booted`) — в стартовом действии узла OTA
  (`begin`), один раз за запуск и не после принятого образа; не возвращать его
  в `main` до исполнителя{% if has_usb == "true" %}; узел `USB` встаёт на шину только после этой попытки{% endif %} —
  `docs/ota.md`, «Подтверждение образа — в узле OTA, один раз за запуск».
{%- endif %}
{%- if ota == "true" and ota_store == "external" %}
- С `ext_flash=yes` (`DFU`/`STATE` на внешней флеши): `EXT_PAGE_SIZE` — одно
  число на `boot` и `bsp::external`, и стирание драйвера обязано его делить
  (`docs/flash.md`); bootloader грузит текущий образ мимо `prepare`, только
  если `STATE` не читается или флеш отказала при проверке подписи, но не на
  журнале после прочитанной магии `Swap` (`Screening::JournalUnreadable` —
  паника и сброс); иначе — строгий `prepare`, не `try_prepare` (`docs/ota.md`,
  «Bootloader и внешняя флеш»).
{%- endif %}
{%- if swap_paged == "true" %}
- `SWAP_PAGE_SIZE` ({{page_size}}) — страница обмена крупнее физической, одно число на
  `boot` и `bsp::flash`; `ACTIVE` и `DFU` стоят на `ErasePage<_, SWAP_PAGE_SIZE>` —
  `docs/flash.md`, «Страница раздела — не всегда страница чипа».
{%- endif %}
{%- if ota == "true" and ota_store != "external" %}
- `STATE_ERASE_SIZE` — страница сектора `STATE`, одно число на `boot` и
  приложение (`bsp::flash::STATE_ERASE_SIZE`){% if config_store == "flash" %}; так же `CONFIG_ERASE_SIZE` у `CONFIG`{% endif %} —
  `docs/flash.md`, «Страница раздела — {% if erase_page == "true" %}не всегда {% endif %}страница чипа».
{%- endif %}
{%- if ota == "true" %}
- Разделы обновления плата отдаёт нарезанными (`board.firmware:
  bsp::FirmwareFlash` = `adapters::ota::Partitions`: `DFU`/`STATE` со своими
  страницами, размер `ACTIVE`, отпечаток раскладки). Режет их `bsp::flash`
  по символам `memory.x` (`bsp::layout::{dfu, state}`) — тем же, что читает
  `from_linkerfile` в bootloader'е; узел OTA собирает над ними
  `adapters::ota::Updater`. Нарезку проверяет только `cargo xtask lint cross`/`build`,
  host-тесты подают адаптеру разделы в памяти — `docs/ota.md`.
{%- endif %}
{%- if ota == "true" or config_store == "flash" or config_store == "external" or dma_nc == "true" %}
- Границы разделов и регионов памяти, нужные рычагам платы, — символы
  `memory.x`, и читает их только `bsp::layout`; проверки над ними — `ASSERT` в
  `memory.x`, не `const`-assert; в `[env]` — только числа, нужные компилятору;
  символы обязаны быть в `memory.x` каждого бинарника, который их читает —
  `docs/flash.md`, «Символы раскладки».
{%- endif %}
{%- if bank_switch != "" %}
- `bsp::banks::ensure_single_bank()` стоит первой строкой {% if ota == "true" %}и в `boot`, и в
  `Board::new`{% else %}`Board::new`{% endif %} — до любого `Flash::new`; не переносить ниже и не убирать —
  `docs/flash.md`, «Банки flash».
{%- endif %}
- Паникёр, обработчик `HardFault` с причиной и ловушка деления — в `app`
  (`crates-cross/app/src/panic.rs`, `fault.rs`), не в `bsp`: с LTO обработчик
  попал бы в любой бинарник, линкующий `bsp` — `docs/clocks.md`, «Ядро: фолты
  и ловушка деления». Сброс МК — рычаг `bsp::reset::Reboot`.
- Флаги сброса `RCC` читает и снимает только `main` приложения — первым делом,
  до `Board::new` (`bsp::reset::take_cause`), один раз за запуск —
  `docs/diagnostics.md`, «Причина сброса».
{%- if cache != "" %}
- `bsp::cache::enable()` — в `Board::new` сразу за `embassy_stm32::init`{% if cache contains "scb_d" %}; буфер
  DMA кладите в `#[link_section = ".dma_nc"]`{% if bdma_nc == "true" %} (BDMA — `".bdma_nc"`){% endif %}, не в обычный `static`:
  кэш данных M7 DMA не видит{% endif %} — `docs/clocks.md`, «Кэши».
{%- endif %}
{%- if cpu_freq_boost == "true" %}
- `bsp::clocks::ensure_cpu_freq_boost()` — первой строкой `Board::new`, до
  `embassy_stm32::init` — `docs/clocks.md`, «`CPU_FREQ_BOOST`».
{%- endif %}
{%- if has_net == "true" %}
{%- if rng_irq != "" %}
- Клок блока `RNG` даёт `config()` в `crates-cross/bsp/src/board.rs` — меняете
  тактирование, сохраните его: без клока прошивка паникует или виснет в
  `Board::new`. Генератор наружу из `bsp` не выходит{% if net_services contains "ssh" %}: SSH берёт байты только
  через `getrandom`{% endif %} — `docs/modules/bsp-rng.md`.
{%- endif %}
{%- endif %}
{%- if protocol == "canopen" %}
- Буфер сохранённых объектов CANopen, колбэки стека над ним и сам стек
  (`Node`) — локальные `domain::canopen::task`, автомат получает их аргументами
  стартового действия (`start(persisted, callbacks, node) => begin` в таблице,
  `ctx.run(&persisted, &mut callbacks, &mut node)`), а в состояниях живёт
  `Stack<'f>` — ссылки на них. Не переносить их в `static` или слот: тип
  замыкания там не назвать, а колбэки `zencan` своих данных не получают.
  Ключи `OBJECTS_KEY` и `NODE_ID_KEY` хранилища настроек заняты узлом{% if config == "true" %}, и носитель
  (`board.config`) уходит ему — хранилищем над ним (`Settings::whole`) во входе, и
  своим настройкам проекта нужен общий доступ через него{% endif %} —
  `domain::canopen`.
{%- endif %}
{%- if dfu_bank == "true" %}
- `DFU` — во втором банке: `boot` и `bsp` пишут его через `Bank2Region`, а
  символы `__bootloader_dfu_*` отсчитаны от начала банка 2; менять только
  вместе — `docs/flash.md`, «Банки flash».
{%- endif %}
{%- if config == "true" %}
- Хранилище настроек — `adapters::settings` над любым async `NorFlash`, на весь
  его объём. Носитель отдаёт плата рычагом `NorFlash`
  (`board.config: bsp::ConfigFlash`): {% if config_store == "eeprom" %}EEPROM L0/L1 (доступ — порт `ports::EepromAccess`, обёртку
  `adapters::eeprom::Eeprom` собирает сама плата){% elsif config_store == "external" %}внешняя флеш W25Q (драйвер
  `w25q` в `bsp::external`, шина — заглушка `bsp::stub::FlashBus`, пока проект не
  подставит выводы){% else %}раздел `CONFIG` внутреннего
  флеша{% endif %}. Само хранилище — `Settings::whole(board.config)` на compose-site
  узла-потребителя; тип — `adapters::settings::Settings<bsp::ConfigFlash>` прямо {% if graph == "true" %}в
  графе{% else %}в задаче{% endif %}{% if config_store == "eeprom" %}. EEPROM под хранилищем инвертирована: без
  этого заводские нули выглядели бы для `sequential-storage` закрытыми
  страницами{% endif %} — `docs/flash.md`.
{%- endif %}
{%- if graph == "true" %}
- У каждого узла с `watchdog:` цепочка таймаутов (период отметок < таймаут
  узла < `bsp::wdg::HW_TIMEOUT_US` минус `WATCHDOG_CHECK_EVERY`) — менять
  только вместе; новый узел с `watchdog:` обязан встать в список `const`-assert'а
  в `crates-cross/app/src/graph.rs` — `docs/watchdog.md`.
- Сторож запускается в прологе `spawn_all` (`= board.watchdog.arm()`), а не в
  `Board::new`{% if ota == "true" %}; в release его раньше запускает bootloader с тем же таймаутом, и путь
  от прыжка до `spawn_all` обязан в него уложиться{% endif %} — `docs/watchdog.md`.
{%- elsif ota == "true" %}
- Сторожа нет ни в `boot`, ни в приложении: зависший до `mark_booted` образ сам
  не откатится.
{%- endif %}
- Цикл исполнителя в `crates-cross/app/src/main.rs` поднят руками, а не
  `#[embassy_executor::main]`: возврат к макросу молча ломает замер загрузки —
  `docs/modules/app-cpu-load.md`, «Почему цикл исполнителя в `main` написан руками».
{%- if protocol_link == "true" %}
- Канал протокола строит `link(…)` модуля транспорта `crate::link::<вид>[::<транспорт>]`
  над рычагом платы, память — {% if graph == "true" %}`static T` в привязке графа{% else %}статики модуля задачи{% endif %}; размер буфера и
  порт — решение `app`, `bsp` их не знает; обработчик протокола — не порт{% if has_protobuf == "true" %};
  канал `PROTOBUF` — `ports::FrameDuplex`, ответ — `send_from` из своего буфера{% endif %} —
  `docs/modules/app.md`, «Канал протокола: `crate::link`».
{%- endif %}
{%- if net_services contains "rest" %}
- Токен REST API — `rest-token.txt` в корне проекта: создаёт `cargo xtask`
  (`secrets::ensure`), и он **коммитится**, а не уходит в `.gitignore` —
  README, «REST API». Число соединений — `HTTP_CONNECTIONS` в `[env]`; пул
  `HTTP[{{ http_connections }}]` в `graph.rs` — литерал того же числа (`const`-assert там же):
  правите руками число — правьте и литерал.
{%- endif %}
{%- if net_services contains "ssh" %}
- Консоль SSH (`domain::ssh`, узел `SSH`): {% if ssh_auth == "password" %}пароль — `ssh-password.txt`{% else %}открытый ключ пользователя — строка `crate::net::ssh::LOGIN`{% endif %}{% if ssh_host_key == "file" %}, ключ хоста — `ssh-host-key.bin`{% endif %}{% if ssh_auth == "password" or ssh_host_key == "file" %};
  файлы создаёт `cargo xtask` (`secrets::ensure`), и они **коммитятся**{% endif %} — README, «Консоль SSH».{% if ssh_host_key == "config" %}
  Ключ хоста плата создаёт при первом старте (ключ настроек
  `domain::ssh::HOST_KEY_ID`) — хранилище настроек забирает узел `SSH`, своим
  настройкам проекта нужен общий доступ через него.{% endif %} Случайность `sunset`
  берёт через `getrandom` (`docs/modules/bsp-rng.md`): без бэкенда ключи сессий
  предсказуемы. Код `sunset` — около 165 KiB, отсюда порог раздела приложения в
  генерации (`SSH_MIN_PARTITION`).
{%- endif %}
{%- if net_services contains "mqtt" %}
- Брокер MQTT (`domain::mqtt`, узел `MQTT`) — `mqtt.toml` в корне проекта:
  создаёт `cargo xtask` (`secrets::ensure`), он **коммитится**, вшивает его
  `crates-cross/app/build.rs` (README, «Клиент MQTT»). Паузы перед попытками
  `dial` — контракт порта `ports::dial::Dial`, их держит `adapters::net::TcpClient`,
  а не `domain`{% if ota == "true" %}; клиент входа `CMD` узла OTA — номер `{{ mqtt_ota_client }}`{% endif %}; доступ к `cmd/` —
  только ACL брокера — `docs/modules/app-mqtt.md`.
{%- endif %}
{%- if graph == "true" %}
- `#![allow(dead_code)]` в начале `crates-cross/app/src/graph.rs` — не
  украшение, а условие `-D warnings`: файл целиком генерируемый, а
  `*_receiver()`/`provide_*`/`*_take()` в сыром шаблоне никто не зовёт, потому
  что подписчиков нет. Снимать allow нельзя, пока у графа нет читателей; писать
  в `graph.rs` свою логику — тоже.
{%- endif %}
- `Board` отдаёт рычаги платы — объекты под трейтами — и данные о чипе
  (`bsp::ChipSpec`) и ничего не настраивает: ни `Peripherals`, ни периферии
  ядра, ни `Clocks` в полях нет; протоколов приложения над рычагами и их буферов
  в `bsp` нет — их строит `app`{% if has_stub == "true" %}. Заглушка периферии до драйвера — `bsp::stub`{% endif %} —
  `docs/architecture.md`, «`Board` отдаёт рычаги платы, а не периферию и не протоколы».
{%- if external_flash == "true" %}
- Внешняя флеш — W25Q, драйвер `w25q` (hw-drivers, `rev` закреплён) в
  `bsp::external`; шина под ним — заглушка `bsp::stub::FlashBus`, пока плата
  не подставит свою (рецепт — `docs/modules/bsp-external.md`).
  `Board::new` асинхронный: драйвер опознаёт микросхему (`init`, до 2 с, если
  она дотирает стирание), ждёт таймерами `embassy_time` и потому зовётся из
  первой задачи исполнителя (`start` в `main.rs`), а не через `block_on` до
  него — там таймер паникует{% if graph == "true" %}; путь до `spawn_all` обязан уложиться в таймаут сторожа{% endif %}.
  Не прошёл `init` или микросхема меньше конца разделов (символ `memory.x`
  `__ext_flash_end`, `bsp::layout::ext_flash_end`) — строка в лог, флеш
  отказывает, а не паникует.
{%- endif %}
- `Board::new()` отдаёт один `Board`{% if has_usb == "true" or has_net == "true" %}; пары, которые нельзя собрать порознь
  ({% if has_usb == "true" %}`bsp::usb::<транспорт>::Usb { device, cdc }`{% if has_net == "true" %}, {% endif %}{% endif %}{% if has_net == "true" %}`bsp::net::Net { runner, stack }`{% endif %}), собирает плата{% endif %}.
  {% if graph == "true" %}Граф берёт `boot: board: bsp::Board;` и делит по полям{% else %}Поля делит `tasks::spawn_all(spawner, board)`{% endif %}. Не делать поля
  `Board` `Option`{% if has_usb == "true" or has_net == "true" %} и не передавать половину пары через `static`{% endif %} — там же.
{%- capture bsp_deps %}{% if flash_page == "true" %}, `flash-page`{% endif %}{% if has_usb == "true" or has_net == "true" or config_store == "eeprom" or ota == "true" %}, обёртки `adapters`{% endif %}{% endcapture %}
- `bsp` зависит от `ports`, HAL и того, из чего собирает свои рычаги{% if bsp_deps != "" %}
  ({{ bsp_deps | remove_first: ", " }}){% endif %}, но не от `domain`: входы узлов из полей `Board`
  заполняет compose-site в `app` — `docs/architecture.md`, «Адаптеры делятся по
  одному признаку — нужен ли чип».
- Liquid в корневом workspace (все его члены и корневой `Cargo.toml`) — только
  внутри комментариев и строковых литералов: в репозитории шаблона эти крейты
  компилируются целиком — `docs/architecture.md`.
- Новую зависимость для `cross` нельзя считать рабочей на тёплом кеше — проверять
  `rm -rf target && cargo build --target {{target}} ...` —
  `docs/conventions.md`.

## Карта документации

«Почему так решено» — по темам, не здесь: `docs/README.md`.

## Перед тем как отчитаться

Не сообщать о завершении задачи, пока все четыре команды из «Порядок проверки
перед коммитом» не прошли (`EXIT=0`) — по одной: так видно, какая именно упала.
