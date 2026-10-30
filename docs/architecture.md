# Архитектура: границы крейтов и что пересекает границу

## Главный принцип: domain — логика, cross — железо

Два независимых Cargo workspace:

- **`domain`** (корень репозитория) — вся бизнес-логика. `no_std`, не знает про
  `embassy-stm32` и ни про какое железо, тестируется на host (`cargo xtask test host`).
- **`crates-cross/`** — аппаратная прошивка STM32 (`bsp`, `app`{% if ota == "true" %}, `boot`{% endif %}). Собирается под
  `{{target}}` (или другой ARM target), подключает `domain` как обычную
  path-зависимость.

**Правило**: `cross` должен оставаться минимальным. `bsp` — рычаги платы:
объекты под трейтами и портами, которыми можно управлять (потоки, шины, разделы
флеша, {% if graph == "true" %}сторож, {% endif %}драйверы микросхем платы), и DMA-буферы, которые нужны железу.
`app` — оркестрация задач (`Spawner`{% if graph == "true" %}, supervisor-графы, watchdog{% endif %}) и статики,
которые домен получает ссылками: буферы узлов и транспортов{% if has_modbus == "true" %}, общая карта
Modbus{% endif %}{% if net_services != "" %}, секреты служб{% endif %}. Адаптеры над рычагами строит `app` и отдаёт узлам
готовыми (ниже). Вся остальная логика — в `domain`, даже если она асинхронная и типа
выглядит «системной» (стейтчарты, RPC, синхронизация задач).
{%- if ota == "true" %}

**Единственное исключение — `boot`.** `app` подключает `domain` целиком, а bootloader
(`crates-cross/boot`) — только ради {% if ota_store == "external" %}двух свободных функций над блокирующими
разделами: `domain::ota::boot::boot_plan` (обменивать ли разделы или грузить
текущий образ) и `domain::ota::boot::screen_update` (проверка подписи перед
обменом). В образ попадают только они{% else %}одной свободной функции над блокирующими
разделами — `domain::ota::boot::screen_update` (проверка подписи перед
обменом). В образ попадает только она{% endif %}: `lto = "fat"` и `--gc-sections`
выбрасывают всё, на что нет ссылок (узлы, `supervisor`, `embassy-executor`). Типы
решения bootloader'а — {% if ota_store == "external" %}`BootPlan`, {% endif %}`Screening`, `ScreenFailure` — живут там же, в
`domain::ota::boot`; логика решения остаётся в `domain` и тестируется на хосте, а не
копируется в `boot`. `bsp` по-прежнему `domain` не зависит (см. ниже).

От `bsp` bootloader зависит всегда: {% if bank_switch != "" %}переключение банков (`bsp::banks`), {% endif %}подъём
HAL (`bsp::board::init_peripherals` — на умолчаниях `embassy-stm32`, не на конфиге
платы), константы раскладки ({% if shared_flash == "true" %}`bsp::flash`{% if external_flash == "true" %}, {% endif %}{% endif %}{% if external_flash == "true" %}`bsp::external`{% endif %}{% if graph == "true" %},
`bsp::wdg::HW_TIMEOUT_US`{% endif %}). Копий у `boot` больше нет. Точка входа
(`cortex-m-rt`) и паникёр у него свои: паника и фолт — решение прошивки, а не
рычаг платы, и у `boot` это сброс МК без сообщения, а у `app` — дамп в
`PANIC` и обработчик `HardFault` с причиной (`app/src/panic.rs`,
`app/src/fault.rs`; в `bsp` обработчик с LTO попадал бы в образ `boot` целиком,
около 14 KiB зоны). Всё, что в `bsp` нужно только бинарнику с исполнителем, — за
фичей, которую включает `app`, а `boot` нет: `time-driver` (драйвер времени
запускал бы TIM и оставлял его прерывание размаскированным на момент прыжка в
приложение). Фичи сливаются по всем крейтам одного вызова cargo, поэтому `boot`
собирается своим (`cargo build -p boot --release`).
{%- endif %}

Как решить, куда класть новую зависимость/код:
- Знает имя чипа или блока, раскладку флеша, создаёт DMA-буфер под железо,
  драйвер микросхемы на плате → `bsp`.
- Заводит память, которую домен получает ссылкой (буфер канала, сокета,
  клиента), строит адаптер над рычагом платы (кадрирование, сокет, хранилище),
  требует `Spawner` → `app`; сама реализация адаптера — `adapters`.
- Всё остальное (FSM, конкурентные примитивы, конфигурация, коллекции, RPC-протокол
  между задачами) → `domain`, даже если результат используется только из `cross`.
{%- if graph == "true" %}

**Узлы графа и их тела — тоже `domain`.** Подсистема объявляет свой узел
`supervisor_fragment!`-ом рядом со своим кодом — самодостаточным модулем: задача,
политика перезапуска, таймаут, слоты, при необходимости собственная `boot:`-привязка
(`domain::heartbeat::HEARTBEAT_FRAG`{% if ota == "true" %}; `domain::ota::OTA_FRAG`, виден как
`::domain::OTA_FRAG`{% endif %}). Граф собственных узлов не объявляет: `crates-cross/app/src/graph.rs`
— compose-site, то есть `boot:`-объект, блок `watchdog:` и список `fragments:`,
который перечисляет фрагменты и даёт узлам имена (`HEARTBEAT_FRAG as HEARTBEAT`{% if ota == "true" %}, `OTA_FRAG<…> as OTA`{% endif %})
— ровно то, что знает про железо. Тот же фрагмент под другим `as` — второй узел.
Держится это на двух возможностях `supervisor`: фрагмент раскрывается в один
`#[macro_export] macro_rules!`-ретранслятор (ни задач, ни статиков, ни `spawn_all` —
всё это делает compose-site), а узел с `ctx:` получает структуру контекста, которую
фрагмент эмитит в своём же крейте, — поэтому сигнатура задачи не называет ничего из
крейта с графом. Так же устроен и рабочий пример в самом `supervisor`
(`supervisor-logic-fixture`).
{%- endif %}

**Каждая задача узла — автомат `#[fsm::typestate]` на {% if graph == "true" %}этом{% else %}своём{% endif %} контексте.** Таблица
состояний и переходов стоит на `impl <Узел>Ctx`{% if graph == "true" or ota == "true" %} ({% if graph == "true" %}`heartbeat::HeartbeatCtx`,
`cpu_load::CpuLoadCtx`{% if ota == "true" %}, {% endif %}{% endif %}{% if ota == "true" %}`ota::OtaCtx`{% endif %}){% endif %}, действия — методы контекста, задача —
`match ctx.run().await {}` или, у автомата с выходным состоянием, `ctx.run().await;`.
Выходное состояние у узлов шаблона только формальное (`exit Down` у узлов, чьё обслуживание
не возвращается): они не падают{% if graph == "true" %} (у них нет `restart:`){% endif %}, а
на отказ связи возвращаются в начальное состояние (переждать отказ — дело
реализации порта). Нелегальный переход — `E0277`,
необработанное событие — `E0004`; второй структуры вокруг контекста нет.{% if graph == "true" %} Данные,
которые должны пережить рестарт, — `resources:` узла, всё остальное — поля
состояний.{% else %} Исключение одно: замер загрузки (`cpu_load::log_load`) — не
узел, контекста у него нет, и он остаётся циклом.{% endif %}

**Логика — функции над портами; типы, которыми она с ними говорит, — в `ports`.**
{% if ota == "true" %}Приём образа (`download::Transfer`), применение обновления (`update::apply_signed`),
версия (`ota::version::*`), решение bootloader'а (`ota::boot::*`) — функции над портами, а действия автоматов зовут их. {% endif %}Всё, чем
логика разговаривает с портами — ошибки, — объявлено в `ports`: тот стал словарём
домена, а не только набором трейтов. Своё в `domain` — только контексты узлов{% if graph == "true" %}
(их эмитит фрагмент), состояния автоматов и входы фрагментов ({% if ota == "true" %}`OtaWiring`, {% endif %}`IdleMeter`{% if ota == "true" %}, `Mode`{% endif %}){% else %} и состояния автоматов{% endif %}{% if protocol != "none" %},
включая обработчик протокола{% if graph == "true" %} на этом входе{% endif %}: {% if protocol == "protobuf" %}`protobuf::Service`{% elsif protocol == "modbus" %}`modbus::EmptyRegisters` (порт `Registers`; карта одна на проект —
псевдоним `crate::modbus::Map`, ручка `crate::modbus::Handle` над
`domain::modbus::MapMutex`, — все узлы `MODBUS_*` работают над ней{% if ota == "true" %}, а окно OTA
`modbus::OtaRegisters` оборачивает ручку снаружи, вне блокировки{% endif %}){% else %}`canopen::dictionary` (словарь объектов){% endif %} — логика приложения,
которую проект правит под себя, поэтому живёт в `domain`, а не в `app`{% endif %}.
Состояние внутри одного вызова функции{% if ota == "true" %} (хвост до слова флеша при приёме){% endif %} живёт в
её локальных переменных, а не в структуре.

**Адаптеры делятся по одному признаку — нужен ли чип.** Реализация порта, которой
хватает трейтов `embedded-storage`/`embedded-hal`/`embedded-io-async` (раздел флеша
или шина приходят параметром), лежит в `crates-host/adapters` и тестируется на
хосте на фейке
{%- assign sep = ": " %}
{%- if ota == "true" %}{{ sep }}`adapters::ota` (поверх `embassy-boot`){% assign sep = ", " %}{% endif %}
{%- if settings_port == "true" %}{{ sep }}`adapters::settings`
(поверх `sequential-storage`){% assign sep = ", " %}{% endif %}
{%- if protocol_link == "true" %}{{ sep }}транспорты протоколов{% endif %}. Строит их **compose-site**, а
не узел и не `bsp`: {% if graph == "true" %}привязка узла в `fragments:` графа — одним выражением над рычагом
платы (`Updater::new(board.firmware)`, `link(board.net.stack, static [u8; N], …)`),
а память адаптера объявляет там же `static T`: ячейка на вхождение, в `.bss`,
мимо стека{% else %}модуль задачи `crates-cross/app/src/tasks/` над рычагом платы, память — его
статики `ConstStaticCell`{% endif %}. Узел получает адаптер готовым во входе и только читает поле
(`LINK: local L = inputs.link`). Трейта сборки и структур буферов у адаптеров нет:
`static` нужного размера можно объявить только там, где тип адаптера
конкретен, — на compose-site.
{%- assign sep = "" %}
{%- capture types %}
{%- if protocol_link == "true" %}{{ sep }}`crate::link::<вид>::<транспорт>::Link`{% assign sep = ", " %}{% endif %}
{%- if ota == "true" %}{{ sep }}`adapters::ota::Updater<bsp::FirmwareFlash, _>`{% assign sep = ", " %}{% endif %}
{%- if config == "true" %}{{ sep }}`adapters::settings::Settings<bsp::ConfigFlash>`{% assign sep = ", " %}{% endif %}
{%- endcapture %}
{%- if types != "" %} Тип адаптера —
{% if graph == "true" %}параметр фрагмента{% else %}в модуле задачи{% endif %}: {{ types }}.
{%- endif %} Та реализация, которой
нужны `embassy_stm32::peripherals` или регистр, — в `crates-cross/bsp`: сброс МК
(`bsp::reset`){% if graph == "true" %}, сторож над IWDG (`bsp::wdg`){% endif %}
{%- if shared_flash == "true" or external_flash == "true" %}, куски флеша со своими страницами ({% if shared_flash == "true" %}`bsp::flash`{% if external_flash == "true" %},
{% endif %}{% endif %}{% if external_flash == "true" %}`bsp::external`{% endif %}){% endif %}
{%- if has_stub == "true" %}, заглушки периферии, пока
нет драйвера (`bsp::stub`){% endif %}{% if erase_page == "true" %}. Страница раздела
(`flash_page::ErasePage`) — свой хостовый крейт, общий у `bsp` и `boot`: это
геометрия флеша, а не адаптер, и обмен `embassy-boot` над ней проверяется на
хосте{% endif %}.
{%- assign sep = "" %}
{%- capture wrappers %}
{%- if has_usb == "true" %}{{ sep }}пару USB (`adapters::usb`){% assign sep = ", " %}{% endif %}
{%- if has_net == "true" %}{{ sep }}пару сети (`adapters::net::Net`){% assign sep = ", " %}{% endif %}
{%- if config_store == "eeprom" %}{{ sep }}EEPROM как `NorFlash`
(`adapters::eeprom::Eeprom`){% assign sep = ", " %}{% endif %}
{%- endcapture %}
{%- if wrappers != "" %} Обратная зависимость есть: `bsp` берёт из `adapters` обёртки,
которые собирает вместе с рычагом сама плата, — {{ wrappers }}; от `domain` `bsp` не зависит.
{%- else %} От `domain` `bsp` не зависит.
{%- endif %}

**Порт и адаптер — или библиотека прямо в `domain`.** Порт в `ports` и его
реализация в `adapters` заводятся не «на случай, если заменим интерфейс» — в
прошивке конкретного изделия этого не случается, — а когда есть хотя бы одна из
трёх причин:

1. **Реализаций несколько одновременно.** Шаблон генерирует проекты с разными
   транспортами и носителями, а в одном проекте протокол может работать над
   несколькими транспортами сразу
{%- if has_modbus == "true" %}: у Modbus кадрирование своё на каждый
{%- assign sep = " (" %}
{%- if modbus_rtu == "true" %}{{ sep }}RTU{% assign sep = ", " %}{% endif %}
{%- if modbus_tcp == "true" %}{{ sep }}MBAP над TCP{% assign sep = ", " %}{% endif %}
{%- if modbus_udp == "true" %}{{ sep }}MBAP в датаграммах UDP{% assign sep = ", " %}{% endif %})
{%- elsif has_protobuf == "true" %}: канал `PROTOBUF` — {% if cobs_used == "true" %}над COBS{% if has_udp == "true" %} и {% endif %}{% endif %}{% if has_udp == "true" %}над датаграммой{% endif %}
{%- endif %}.
2. **Логике нужен фейк с отказами.** Алгоритм узла проверяется на хосте отказом
   в нужный момент{% if ota == "true" %} — «подтверждение образа не удалось, повторить на следующем
   `Start`» (`domain::ota` над фейком `FirmwareUpdate`){% endif %}; на настоящей библиотеке
   такой отказ на хосте не воспроизвести.
3. **Тяжёлая зависимость отделяется от логики**, чтобы алгоритм читался и
   тестировался без неё
{%- assign sep = " (" %}
{%- if ota == "true" %}{{ sep }}`embassy-boot`{% assign sep = ", " %}{% endif %}
{%- if settings_port == "true" %}{{ sep }}`sequential-storage`{% assign sep = ", " %}{% endif %}
{%- if has_net == "true" %}{{ sep }}сокеты
   `embassy-net`{% assign sep = ", " %}{% endif %}
{%- if sep == ", " %}){% endif %}.

Нет ни одной — порт был бы пустой прослойкой, и `domain` зовёт библиотеку прямо:
реализация одна и тестируется как есть
{%- assign sep = " (" %}
{%- if has_protobuf == "true" %}{{ sep }}`micropb`{% assign sep = ", " %}{% endif %}
{%- if net_services contains "mqtt" %}{{ sep }}`minimq`{% assign sep = ", " %}{% endif %}
{%- if net_services contains "ssh" %}{{ sep }}`sunset`{% assign sep = ", " %}{% endif %}
{%- if net_services contains "rest" %}{{ sep }}`picoserve`{% assign sep = ", " %}{% endif %}
{%- if has_canopen == "true" %}{{ sep }}`zencan`{% assign sep = ", " %}{% endif %}
{%- if sep == ", " %}){% endif %}. Цены у порта в прошивке нет — обобщённый код
мономорфизуется, косвенных вызовов не появляется, — так что решает только то,
есть ли за ним вторая реализация или тест.

Та же мерка — для датчиков и микросхем, которых в шаблоне ещё нет: драйвер —
`bsp` (рычаг платы); порт — `ports`, если у логики над датчиком есть что
проверять отдельно или датчиков одного назначения несколько; адаптер «порт над
драйвером» — `adapters`. Светодиоду, у которого проверять нечего, хватает трейта
`embedded-hal`: свой порт ему не нужен.
{%- if graph == "true" %}

Цифры узла — тоже в `domain`: `domain::heartbeat::WATCHDOG` фрагмент называет
полным путём `$crate::heartbeat::…` (токены `macro_rules!` резолвятся на compose-site, и
голое имя искалось бы в `graph.rs`), так что от compose-site он не требует ни одного
объявления. Левое звено цепочки таймаутов — период отметок меньше таймаута узла —
при этом целиком внутри фрагмента, и его держит `const`-assert в `domain::heartbeat`;
compose-site остаётся правое: таймаут узла плюс `WATCHDOG_CHECK_EVERY` меньше
аппаратного `bsp::wdg::HW_TIMEOUT_US` — его тоже держит `const`-assert, уже в
`graph.rs`, потому что это единственное место, где видны оба конца
(`docs/watchdog.md`).
{%- if ota == "true" %}

**Типы ресурсных слотов** узла OTA — параметры фрагмента: `name: OTA_FRAG<F, R>`,
а compose-site подставляет их в `fragments:` угловыми скобками
(`::domain::OTA_FRAG<::adapters::ota::Updater<bsp::FirmwareFlash, true>, bsp::reset::Reboot>`). Слот — `static`, тип ему
нужен, а конкретный тип знает только `cross`; generic-`static` не бывает, и
параметр — единственный способ фрагмента назвать тип, которого он не видит.
Bound'ов у параметров нет намеренно: их держит сигнатура `ota::task`, и rustc
проверяет её, а не копию в DSL. Слот адаптера (`FLASH`) к тому же помечен `local`,
потому что `Updater` над флешем `bsp::FirmwareFlash` — `!Send` (держит `&'static Mutex<NoopRawMutex,
…>`), а обычный слот — `static`, которому нужен `Send`; `local` снимает это
требование, но взамен обязывает держать значение на одном исполнителе — том,
что спавнит граф, и не даёт перевести узел, чей `local`-слот заполнен
инициализатором (как здесь), на другой (`executor:`). Плата за это — фича
`supervisor/local-resources` в `crates-cross/Cargo.toml` и по `unsafe`-блоку
на каждый такой `local`-`static`, который макрос эмитит в `app`. Это то же
допущение «один исполнитель», что у `NoopRawMutex` в `bsp::flash::FlashMutex`, —
менять их вместе (`docs/flash.md`).{% if has_modbus == "true" %} Общая карта узлов Modbus из него вышла:
`domain::modbus::MapMutex` — `CriticalSectionRawMutex`, потому что карта лежит
в `OnceLock` в `static` (`crate::modbus`), а тому нужен `Sync`.{% endif %}
{%- endif %}

**Большой объект — слот `&'static mut T = static`, а не по значению.** Слот по
значению дважды проходит через стек (пролог `spawn_all` и первый опрос задачи)
и живёт дважды (в слоте и в future задачи), а стек под `flip-link` — остаток
RAM. От килобайта и выше фрагмент пишет `resources: [X: &'static mut T = static]`:
статик в `.bss` заводит граф, `T` реализует `supervisor::Zeroed` —
`docs/modules/app-graph.md`.
{%- endif %}

Цена решения «узлы — в `domain`» записана явно: `domain` зависит от `supervisor` ради двух типов в
сигнатуре (контекст узла, `Heartbeat`), а вместе с ним транзитивно от `embassy-executor`,
`embassy-time` и `embassy-sync`. Прежнее правило «в `domain` нет embassy» отменено
сознательно — иначе задача либо оставалась бы в `cross` (то есть логика жила бы рядом с
железом), либо разговаривала с {% if graph == "true" %}графом{% else %}`supervisor`{% endif %} через собственные порты-обёртки, которых нужно по
одной на каждое поле контекста. Что НЕ отменено: `embassy-stm32` в `domain` по-прежнему
нет и быть не может, и host-тесты `domain` обязаны оставаться зелёными — платформу для
исполнителя и драйвер времени им дают dev-зависимости (`platform-std`, `mock-driver`,
см. `crates-host/domain/Cargo.toml`).

Если сомневаетесь — смотрите прецедент (обоснование целиком в doc-комментарии, не
повторяется здесь): `fsm` (`#[fsm::typestate]`) — раздел «Стейтчарты сюда не входят» в
`docs/modules/app.md`.{% if graph == "true" %} Пограничный случай в ту же сторону — `watchdog`: сам
по себе он чистая логика без единой зависимости, но объявлен в `cross`, потому что
существует ради драйва аппаратного IWDG и без реализации его трейтов под конкретный
МК бесполезен.{% endif %}

## Третий крейт: `chip-info` (справочник по чипу)

`crates-host/chip-info/` — хостовый инструмент, `cargo xtask pins [БЛОК|ПИН]`: выводы с
альтернативными функциями, DMA-каналы, источник тактирования и прерывания
выбранного чипа. Данные — `stm32_metapac::metadata::METADATA`, тот же
`stm32-data`, из которого `embassy-stm32` генерирует типы.

Два решения здесь осознанные, не переигрывать без запроса:

1. **Свой `[workspace]`, а не член корневого.** `stm32-metapac` выбирает чип
   Cargo-фичей (его `build.rs` паникует, если фич `stm32*` не ровно одна, —
   env-переменной это не обойти), фича подставляется при генерации, то есть в
   `[dependencies]` живёт Liquid. Манифест с Liquid не резолвится `cargo
   metadata`, и будь крейт членом корневого workspace, в самом репозитории
   шаблона сломались бы `cargo xtask lint` и `test host`. По этой же причине
   `cargo xtask pins` в репозитории шаблона намеренно падает с объяснением, а
   проверяется только генерацией (`template-check`).
2. **Никакой генерации кода — только запросы к данным.** Предложение «описать
   плату в `board.toml` и генерировать распиновку» уже рассматривалось и
   отклонено: распиновку проверяет компилятор (`SpiSck<SPI1>` для чужого пина
   не реализован, `Peri` не `Copy` — один вывод не отдать двум задачам,
   чип-фича `stm32-metapac` пакетная — пина не из корпуса нет и в типах).
   Генератор дублировал бы эти проверки и завёл второй источник правды.
   Справочник закрывает ровно то, чего компилятор не знает: какие выводы умеют
   нужный сигнал, не занят ли отладочный порт или кварц, от чего тактируется
   блок.

   `cargo xtask pins БЛОК --snippet` этому не противоречит: он **печатает**
   заготовку `bind_interrupts!`/`assign_resources!` в терминал, а не пишет
   файл. Вставляет её человек, дальше она обычный код проекта — второго
   источника правды не появляется. Таблица обработчиков прерываний
   (`HANDLERS` в `chip-info`) сверена с `embassy-stm32` 0.6 и при её
   обновлении требует перечитывания; это подсказка, решает компилятор.

   `cargo xtask pins --check` читает текстом все исходники
   `crates-cross/bsp/src` (`assign_resources!` — макрос, до раскрытия там
   токены, а не дерево; каталогом, а не одним файлом, потому что объявить
   группы проект волен где угодно) и ловит единственное, что не ловит
   компилятор: занятый отладочный порт (ненулевой код возврата) и выводы
   кварца/MCO (предупреждение).

Смена чипа в существующем проекте требует правки двух мест: `crates-cross/Cargo.toml`
и `crates-host/chip-info/Cargo.toml`. Расхождение не молчаливое — `chip-info` сверяет свою
чип-фичу с `crates-cross/Cargo.toml` и предупреждает.

## `Board` отдаёт рычаги платы, а не периферию и не протоколы

Границу `bsp` → `app` пересекают рычаги платы — объекты, которыми можно
управлять, под трейтами:
{%- if has_uart == "true" or has_usb == "true" %} `embedded-io-async` ({% if has_uart == "true" %}поток UART{% if has_usb == "true" %}, {% endif %}{% endif %}{% if has_usb == "true" %}поток CDC{% endif %}),{% endif %}
{%- if ota == "true" or config == "true" %}
`embedded-storage(-async)` ({% if ota == "true" %}флеш под обновлением{% if config == "true" %}, {% endif %}{% endif %}{% if config == "true" %}носитель настроек{% endif %}),{% endif %} порты из
`crates-host/ports` там, где стандартного трейта нет ({% if has_can == "true" %}`CanBus`, {% endif %}`CycleCounter`{% if ota == "true" %}, `Reboot`{% endif %}), — и драйверы микросхем платы
({% if external_flash == "true" %}внешняя флеш W25Q; так же лягут{% else %}так лягут{% endif %} датчики и светодиоды). `Peripherals`,
отдельные пины и группы `assign_resources!` наружу из `bsp` не выходят: их
потребляет сам `Board::new`, собирая рычаги. Приложение работает через трейты
и про регистры не знает.

Протоколов над рычагами `bsp` не собирает: кадрирование транспорта, буферы
сообщений{% if has_net == "true" %}, сокеты протоколов и служб{% endif %}{% if ota == "true" %}, адаптер обновления{% endif %}{% if config == "true" %}, хранилище настроек{% endif %} —
решение проекта, а не свойство платы. Их строит `app` (выше) в своей памяти:
захотел проект другой буфер или порт — правится
`app`{% if protocol_link == "true" or net_services != "" %} ({% if protocol_link == "true" %}`crates-cross/app/src/link/`{% if net_services != "" %}, {% endif %}{% endif %}{% if net_services != "" %}`crates-cross/app/src/net.rs`{% endif %}){% endif %}, плата не меняется. Буфер
заводится там, где решают его размер, — в `app`, — а узел получает ссылку.

`Board` один (`Board::new() -> Board`)
{%- if has_usb == "true" or has_net == "true" %}, и пары, которые нельзя собрать порознь,
собирает сама плата:{% if has_usb == "true" %} USB — `board.<транспорт>: bsp::usb::<транспорт>::Usb {
device, cdc }` (классы `embassy-usb` регистрируются до постройки устройства;
устройство — узлу `USB_*`, поток CDC — транспорту протокола){% endif %}{% if has_usb == "true" and has_net == "true" %} и{% endif %}{% if has_net == "true" %} сеть —
`board.net: bsp::net::Net { runner, stack }` (их рождает один `embassy_net::new`;
фоновая работа — узлу `NET`, стек — сокетам узлов). Сокетов в стеке —
`bsp::net::SOCKETS`: стек строит плата, поэтому и счёт у неё (DHCP-клиент,
сетевые транспорты, соединения служб — числа из генерации), и отдельной
проверки, что слотов хватит, не нужно.{% if rng_irq != "" %} Затравка стека{% if net_services contains "ssh" %} и бэкенд
`getrandom`{% endif %} над RNG — тоже плата (`bsp::rng`).{% endif %}{% else %}.{% endif %}
{%- else %}.
{%- endif %} {% if graph == "true" %}Граф берёт `Board`
`boot:`-объектом (`boot: board: bsp::Board;`) и делит по полям{% if has_usb == "true" or has_net == "true" %}
({% if has_usb == "true" %}`board.<транспорт>.device`/`.cdc`{% if has_net == "true" %}, {% endif %}{% endif %}{% if has_net == "true" %}`board.net.runner`/`.stack`{% endif %}){% endif %}{% else %}Целиком
`Board` берёт `tasks::spawn_all(spawner, board)`{% endif %}. Отвергнуто: `Option`-поля в
`Board` (тип врёт, ошибка уходит в рантайм){% if has_usb == "true" %}, передача половины пары через
`static` (поток CDC не `Send`){% endif %} и отдельные `Drivers` с парами, собранными в
`app`, — так было раньше, и `app` строил объекты, которые по смыслу — рычаг
платы. Прямое следствие того же правила: `embassy-stm32` нет и в манифесте `app` —
даже драйвер времени (`time-driver-any`) приложение включает фичей
`bsp/time-driver`.
{%- if graph == "true" %}

Исключение из «только порты» ровно одно, и то формальное — поле `watchdog` в
проекте с графом задач: сторож реализует не порт из `ports`, а
`watchdog::HardwareWatchdog`. Приём границы тот же (приложение зовёт трейт, а
не регистры), просто трейт приходит из крейта сторожа — держать его копию в
`ports` было бы вторым объявлением одного и того же.

Отдаётся он **заряженным, но не запущенным** (`wdg::UnarmedBoardWatchdog`), а
запускает его инициализатор блока `watchdog:` графа —
`= board.watchdog.arm()`. Причина — в моменте: с этого вызова сторож тикает, а
кормит его только тикер графа, и всё, что стоит между этими двумя точками,
обязано уложиться в аппаратный таймаут. Короче всего промежуток в прологе
`spawn_all`, туда выражение и попадает; запусти его `Board::new` — он тикал бы
всю инициализацию, а в target-тестах, где графа нет, сбрасывал бы плату посреди
теста. Забыть `arm()` нельзя: `HardwareWatchdog` реализован только у
запущенного типа, а блок `watchdog:` требует именно его (`docs/watchdog.md`).
{%- endif %}
{%- if ota == "true" %}

{% if graph == "true" %}Кроме сторожа в граф уезжает поле `firmware`{% else %}В задачу узла OTA уезжает поле `firmware`{% endif %} (`bsp::FirmwareFlash` —
`adapters::ota::Partitions`: разделы `DFU` и `STATE` со своими страницами, предел
образа, равный `ACTIVE`, и отпечаток раскладки). {% if graph == "true" %}Compose-site кладёт его во вход узла,
`domain::ota::OtaWiring { flash: Updater::new(board.firmware), reboot: bsp::reset::Reboot, mode }`,
прямо в `fragments:`: адаптер{% else %}Адаптер{% endif %} обновления (`adapters::ota::Updater`, порт
`FirmwareUpdate`) над ним строит compose-site. Режет разделы плата — по символам
`memory.x` (`bsp::layout`), тем же, что читает bootloader: где они лежат, знает
раскладка, а она — дело платы. Сам `bsp` про `domain` не знает: логика приёма и
применения образа живёт в `domain::ota` и видит только порты; host-тесты подают
адаптеру разделы в памяти, а саму нарезку проверяет сборка `crates-cross`. Образ приходит не по каналу,
а командами узла протокола на вход узла `CMD` ({% if graph == "true" %}`request:` графа, клиент —
`ota_cmd_requester()`{% else %}`static` в `crate::tasks::ota`{% endif %}; `docs/ota.md`). Вместимость раздела адаптер отдаёт через
порт же (`FirmwareUpdate::capacity`). Версия образа — свойство прошивки, а не
платы: её задаёт приложение (`crates-cross/app/src/version.rs`) режимом
узла `Mode::Signed`{% if graph == "true" %} прямо в `fragments:`{% endif %}; ключ подписи — в
bootloader'е (`crates-cross/boot`), который подпись и проверяет.
{%- endif %}

**`Board::new` ничего не настраивает.** Он поднимает HAL (`embassy_stm32::init`
— иначе `Peripherals` взять негде) и собирает объекты. {% if graph == "true" %}Ни сторож не
взводится, ни периферия{% else %}Периферия не{% endif %} конфигурируется «на будущее»:
что делать с объектом, решает приложение, а не `bsp`. Отсюда и имя `new` вместо
прежнего `init`.

Исключения, помимо `ensure_*` для option bytes: {% if cache != "" %}`bsp::cache::enable()` включает
кэши чипа сразу за `init`, {% endif %}`Cycles::new` — счётчик тактов (`docs/clocks.md`,
«Кэши», «Ядро»). Это не «на будущее»: с завода кэши выключены, счётчик стоит, а
знает, что есть у этого ядра, только `bsp`. Ловушку деления включает не плата, а
`app` (`app/src/fault.rs`, первой строкой `main`). Периферию ядра
(`DWT`, `SCB`) они берут через `steal()`, не трогая флаг `take()`.
{%- if external_flash == "true" %}

И внешняя флеш: её драйвер (`bsp::external`) в `Board::new` опознаёт микросхему
— `init` читает JEDEC ID и{% if ext_flash_bus != "spi" %} ставит бит QE, без чего{% else %} без этого{% endif %} драйвер не знает объёма и
не годится ни для одной операции. `init` ждёт микросхему (до 2 с, если она
ещё стирает после сброса), поэтому `Board::new` асинхронный, а `main` до
исполнителя зовёт его через `embassy_futures::block_on`; не прошёл —
строка в лог, и флеш отказывает, а не паникует (docs/modules/bsp-external.md).
{%- endif %}

**Данные — сведения о чипе, и только они.** `spec: bsp::ChipSpec` — имя,
заводской UID, объём flash, частота `sys` — лежит полем `Board`, хотя порта не
реализует: это то, что приложение сообщает о себе наружу (лог старта, ответ
хосту), а знает это только `bsp`. Тип объявлен в `bsp`, рядом с `Board`, а не
в `ports`: домен его не читает, а порт заводится под то, чем пользуется
логика. Понадобятся эти сведения домену — тогда тип и переедет в `ports`.

Целиком `rcc::Clocks` полем не хранится: это тип HAL, и состав его полей
генерируется под чип. `Board::new` печатает его в лог при старте, а `sys`
числом кладёт в `ChipSpec` — их читает target-тест `clocks_match_intent`, см. `docs/clocks.md`.

**Когда заводить порт, а когда обойтись без него.** Порт — под то, чем может
пользоваться логика домена. Причина прошлой паники домену не нужна, это диагностика
старта, — поэтому её печатает `main` напрямую из `panic-persist`, а у `Board` метода
под неё нет.
{%- if ota == "true" %}

**Применение обновления — тоже через порт.** `mark_updated` — метод
`FirmwareUpdate`; запись о подписи (`ports::PendingImage`) кладётся обычным
`write` в запасную страницу `DFU`, отдельного трейта под подпись нет. Порядок
проверок перед вызовом — `domain::ota::update::apply_signed`, и он
host-тестируем. Саму подпись
проверяет bootloader (`domain::ota::boot::screen_update`) — `docs/ota.md`.
{%- endif %}

**Дублировать методы портов собственными нельзя{% if ota == "true" or settings_port == "true" %}.** У {% if ota == "true" %}`adapters::ota::Updater`{% if settings_port == "true" %} и
{% endif %}{% endif %}{% if settings_port == "true" %}`adapters::settings::Settings`{% endif %} {% if settings_port == "true" %}inherent-методы `write`/`read`{% if ota == "true" %}/`mark_booted`{% endif %} были
удалены, а не оставлены{% else %}inherent-метод `mark_booted` был
удалён, а не оставлен{% endif %} рядом с трейтовыми: собственный{% else %}:** собственный{% endif %} метод с тем же именем
перекрывает трейтовый при вызове, и приложение снова начало бы работать с
конкретным типом мимо порта — то есть мимо той самой границы.

**Liquid в корневом workspace — внутри комментариев.** `domain`, `ports`{% if protocol == "protobuf" %}, `adapters`
и `proto`{% elsif protocol == "canopen" %}, `adapters` и `canopen`{% else %} и `adapters`{% endif %} — члены корневого workspace, и их исходники компилируются и тестируются
в самом репозитории шаблона (`cargo xtask lint`/`test host`). Условное подключение — то,
что зависит от ответа при генерации (невыбранный протокол уходит вместе со своим
крейтом и ссылками на него), — пишется Liquid'ом в комментариях:
теги `if`/`endif` в строках-комментариях — `//` в `.rs`, `#` в `.toml`. В репозитории
это обычные комментарии, и проверяются все ветки
сразу; в проекте Liquid вырезает невыбранное, а файлы и крейты целиком удаляет хук. Стек
протокола связи — порт, адаптер, узел, крейт примера и их зависимости — есть только в
проекте, где протокол выбран, и только его: проект без протокола не несёт ничего
из них. Модуль, который нужен любому проекту, объявлен безусловно
(`domain::cpu_load` есть в каждом).

**Задачи embassy не могут быть generic.** Если объект уезжает в задачу, его типу
нужен псевдоним — в сигнатуре `#[embassy_executor::task]`{% if graph == "true" %} и у слота узла{% endif %} нужен
конкретный тип.
{%- assign sep = "" %}
{%- capture levers %}
{%- if has_uart == "true" %}{{ sep }}`pub type Serial = ...`{% assign sep = ", " %}{% endif %}
{%- if has_usb == "true" %}{{ sep }}`bsp::usb::<транспорт>::Device`/`Cdc`{% assign sep = ", " %}{% endif %}
{%- if has_net == "true" %}{{ sep }}`bsp::net::Stack`{% assign sep = ", " %}{% endif %}
{%- if ota == "true" %}{{ sep }}`bsp::FirmwareFlash`{% assign sep = ", " %}{% endif %}
{%- if config == "true" %}{{ sep }}`bsp::ConfigFlash`{% assign sep = ", " %}{% endif %}
{%- endcapture %}
{%- assign sep = "" %}
{%- capture concrete %}
{%- if ota == "true" %}{{ sep }}`adapters::ota::Updater<bsp::FirmwareFlash, _>`{% assign sep = ", " %}{% endif %}
{%- if config == "true" %}{{ sep }}`adapters::settings::Settings<bsp::ConfigFlash>`{% assign sep = ", " %}{% endif %}
{%- endcapture %} Рычагу платы его даёт `bsp` ({% if levers != "" %}{{ levers }}{% else %}`pub type X = ...`{% endif %})
{%- if protocol_link == "true" or concrete != "" %}, адаптеру —{% if protocol_link == "true" %} модуль
`app`, который выбирает его тип (`crate::link::<вид>::<транспорт>::Link` и `Source`){% if concrete != "" %},
или{% endif %}{% endif %}{% if concrete != "" %} конкретный тип прямо во входе узла ({{ concrete }} — модуля под одну строку `app`
не заводит){% endif %}{% endif %}. Приложение при этом всё
равно зовёт только методы порта.

