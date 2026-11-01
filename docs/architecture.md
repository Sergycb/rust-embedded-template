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
флеша, {% if graph == "true" %}сторож, {% endif %}объекты драйверов микросхем платы), и DMA-буферы, которые нужны железу.
`app` — оркестрация задач (`Spawner`{% if graph == "true" %}, supervisor-графы, watchdog{% endif %}) и статики,
которые домен получает ссылками: буферы узлов и транспортов{% if has_modbus == "true" %}, общая карта
Modbus{% endif %}{% if net_services != "" %}, секреты служб{% endif %}. Адаптер, чей тип или память выбирает приложение,
строит `app` и отдаёт узлу готовым; адаптер, без которого рычаг не собрать, —
сама плата (ниже). Вся остальная логика — в `domain`, даже если она асинхронная и типа
выглядит «системной» (стейтчарты, RPC, синхронизация задач).
{%- if ota == "true" %}

**Единственное исключение — `boot`.** `app` подключает `domain` целиком, а bootloader
(`crates-cross/boot`) — только ради одной свободной функции над блокирующими
разделами — `domain::ota::boot::screen_update` (проверка подписи перед
обменом). В образ попадает только она: `lto = "fat"` и `--gc-sections`
выбрасывают всё, на что нет ссылок (узлы, `supervisor`, `embassy-executor`). Типы
решения bootloader'а — `Screening`, `ScreenFailure` — живут там же, в
`domain::ota::boot`; логика решения остаётся в `domain` и тестируется на хосте, а не
копируется в `boot`. `bsp` по-прежнему `domain` не зависит (см. ниже).

От `bsp` bootloader зависит всегда: {% if bank_switch != "" %}переключение банков (`bsp::banks`), {% endif %}подъём
HAL (`bsp::board::init_peripherals` — на умолчаниях `embassy-stm32`, не на конфиге
платы), константы раскладки ({% if shared_flash == "true" %}`bsp::flash`{% if external_flash == "true" %}, {% endif %}{% endif %}{% if external_flash == "true" %}`bsp::external`{% endif %}{% if graph == "true" %},
`bsp::wdg::HW_TIMEOUT_US`{% endif %}). Своих копий этих констант у `boot` нет. Точка входа
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
  строит объект драйвера микросхемы на плате → `bsp`.
- Реализует трейт (порт или стандартный) поверх трейтов `embedded-hal`/
  `embedded-io-async`/`embedded-storage`, без чипа, — в том числе тип драйвера
  микросхемы, если ему хватает этих трейтов, — → `adapters`.
- Заводит память, которую домен получает ссылкой (буфер канала, сокета,
  клиента), строит адаптер, чей тип или память выбирает приложение
  (кадрирование, клиент, хранилище), требует `Spawner` → `app`.
- Всё остальное (FSM, конкурентные примитивы, конфигурация, коллекции, RPC-протокол
  между задачами) → `domain`, даже если результат используется только из `cross`.

### Узлы — фрагменты в `domain`

Узлы и их тела — тоже `domain`. Подсистема объявляет свой узел
`supervisor_fragment!`-ом рядом со своим кодом — самодостаточным модулем: задача,
{% if graph == "true" %}таймаут сторожа, {% endif %}слоты, при необходимости собственная `boot:`-привязка
({% if graph == "true" %}`domain::heartbeat::HEARTBEAT_FRAG`{% else %}`domain::<узел>::<УЗЕЛ>_FRAG`{% endif %}{% if ota == "true" %}; `domain::ota::OTA_FRAG`, виден как
`::domain::OTA_FRAG`{% endif %}). Всё, что фрагмент называет, пишется полным путём
(`$crate::…`, `::supervisor::…`): токены `macro_rules!` резолвятся на
compose-site, а не в крейте, где фрагмент написан, и голое имя искалось бы там
и падало бы с `E0425`. `$crate::` указывает в крейт фрагмента и переживает
переименование зависимости в чужом манифесте, а `crate::` означал бы крейт,
который фрагмент вызывает. Тип, которого фрагмент назвать не может (адаптер над
рычагом в слоте), — его параметр{% if ota == "true" %} (`name: OTA_FRAG<F, R, const PARKED: usize>`){% endif %}. Слово DSL без своей фичи
`supervisor` не компилируется: новое слово во фрагменте — фича в
`crates-host/domain/Cargo.toml`.
{%- if graph == "true" %}

Граф собственных узлов не объявляет: `crates-cross/app/src/graph.rs`
— compose-site, то есть `boot:`-объект, блок `watchdog:` и список `fragments:`,
который перечисляет фрагменты и даёт узлам имена (`HEARTBEAT_FRAG as HEARTBEAT`{% if ota == "true" %}, `OTA_FRAG<…> as OTA`{% endif %})
— ровно то, что знает про железо. Имя узлу даёт запись `fragments:`, а не
фрагмент; тот же фрагмент под другим `as` — второй узел (`docs/modules/app-graph.md`).
Держится это на двух возможностях `supervisor`: фрагмент раскрывается в один
`#[macro_export] macro_rules!`-ретранслятор (ни задач, ни статиков, ни `spawn_all` —
всё это делает compose-site), а узел с `ctx:` получает структуру контекста, которую
фрагмент эмитит в своём же крейте, — поэтому сигнатура задачи не называет ничего из
крейта с графом. Так же устроен и рабочий пример в самом `supervisor`
(`supervisor-logic-fixture`).
{%- else %}

Compose-site — модули задач (подмодули `crates-cross/app/src/tasks.rs`): они
подставляют типы и раздают поля платы.
{%- endif %}

### Каждая задача узла — автомат `#[fsm::typestate]`

Автомат стоит на контексте, который эмитит фрагмент ({% if graph == "true" %}`ctx: $crate::heartbeat::HeartbeatCtx, task: $crate::heartbeat::task;`{% else %}`ctx: $crate::<узел>::<Узел>Ctx, task: $crate::<узел>::task;`{% endif %}). Таблица
состояний и переходов стоит на `impl <Узел>Ctx`{% if graph == "true" or ota == "true" %} ({% if graph == "true" %}`heartbeat::HeartbeatCtx`,
`cpu_load::CpuLoadCtx`{% if ota == "true" %}, {% endif %}{% endif %}{% if ota == "true" %}`ota::OtaCtx`{% endif %}){% endif %}, действия — методы контекста, задача —
`match ctx.run().await {}` или, у автомата с выходным состоянием, `ctx.run().await;`.
Выходное состояние у узлов шаблона только формальное (`exit Down` у узлов, чьё
обслуживание не возвращается); бесконечное обслуживание внутри действия, которое
ведёт в `exit Down`, — можно. Состоянию, которое заимствует данные из кадра
задачи, их дают аргументы стартового действия (`start(..) => begin`,
`ctx.run(&буфер, …)`{% if has_canopen == "true" %}, как у `domain::canopen`{% endif %}), а не `static`. Нелегальный переход — `E0277`,
необработанное событие — `E0004`. Не пишутся: голый `loop` в функции `task`
узла (вне действий автомата), проекция аргументов (`task: f(ctx.поле)` —
контекста она не даёт, автомату стоять не на чем) и вторая структура вокруг
контекста. Состояние, которое живёт дольше прогона автомата, — `resources:`
узла (сброс — в стартовом действии){% if graph != "true" %}: слоты объявляет фрагмент, а заполняет модуль задачи
локальными переменными своего кадра{% endif %}, всё остальное — поля состояний.{% if graph != "true" %} Исключение одно: замер загрузки (`cpu_load::log_load`) — не
узел, контекста у него нет, и он остаётся циклом.{% endif %}

### Узлы не падают

`restart:` нет ни у одного узла, задача возвращает `()`. Отказ связи (канал,
шина, транспорт) — запись в лог и возврат в начальное состояние автомата или
переход в себя, а не выход и перезапуск{% if graph == "true" %} графом{% endif %}; отдельного состояния
восстановления и пауз в `domain` нет. Переждать отказ (не вернуть ошибку снова
без ожидания) — контракт порта, его держит реализация{% if net_services contains "mqtt" %} (паузы перед попытками
подключения к брокеру — `adapters::net::TcpClient`, а не `domain::mqtt`){% endif %}.

### Логика — функции над портами; типы, которыми она с ними говорит, — в `ports`

{% if ota == "true" %}Приём образа (`download::Transfer`), применение обновления (`update::apply`),
версия (`ota::version::*`), решение bootloader'а (`ota::boot::*`) — функции над портами, а действия автоматов зовут их. {% endif %}Всё, чем
логика разговаривает с портами — ошибки, — объявлено в `ports`: тот стал словарём
домена, а не только набором трейтов. Своё в `domain` — только контексты узлов{% if graph == "true" %}
(их эмитит фрагмент), состояния автоматов и входы фрагментов ({% if ota == "true" %}`OtaWiring`, {% endif %}`IdleMeter`){% else %} и состояния автоматов{% endif %}{% if protocol != "none" %},
включая обработчик протокола{% if graph == "true" %} на этом входе{% endif %}: {% if protocol == "protobuf" %}`protobuf::Service`{% elsif protocol == "modbus" %}`modbus::EmptyRegisters` (трейт домена `modbus::Registers`, не порт; карта одна на все узлы `MODBUS_*` —
`docs/modules/app.md`){% else %}`canopen::dictionary` (словарь объектов){% endif %} — логика приложения,
которую проект правит под себя, поэтому живёт в `domain`, а не в `app`{% endif %}.
Состояние внутри одного вызова функции{% if ota == "true" %} (хвост до слова флеша при приёме){% endif %} живёт в
её локальных переменных, а не в структуре.

### Адаптеры делятся по одному признаку — нужен ли чип

Реализация трейта —
порта или стандартного, — которой хватает трейтов
`embedded-storage`/`embedded-hal`/`embedded-io-async` (раздел флеша, шина или
поток приходят параметром), и типы-сборки, которые плата отдаёт целиком, лежат
в `crates-host/adapters` и тестируются на хосте на фейке
{%- assign sep = ": " %}
{%- if ota == "true" %}{{ sep }}`adapters::ota` (поверх `embassy-boot`){% assign sep = ", " %}{% endif %}
{%- if settings_port == "true" %}{{ sep }}`adapters::settings`
(поверх `sequential-storage`){% assign sep = ", " %}{% endif %}
{%- if protocol_link == "true" %}{{ sep }}транспорты протоколов{% endif %}.

**Кто строит адаптер, тот держит его память.** Адаптер, без которого рычаг не
собрать, строит плата в `Board::new`, и его память — статики `bsp` (ниже).
Адаптер, чей тип или память
выбирает приложение, строит **compose-site** — место, где фрагмент получает
типы и входы, — а не узел и не `bsp`: {% if graph == "true" %}привязка узла в `fragments:` графа — одним выражением над рычагом
платы (`Updater::new(board.firmware)`, `link(board.net.stack, static [u8; N], …)`),
а память адаптера объявляет там же `static T`: ячейка на вхождение, в `.bss`,
мимо стека{% else %}модуль задачи (подмодуль `crates-cross/app/src/tasks.rs`) над рычагом платы, память — его
статики{% endif %}. Узел получает адаптер готовым во входе и только читает поле
(`LINK: local L = inputs.link`). Трейта сборки и структур буферов у таких адаптеров нет:
`static` нужного размера можно объявить только там, где тип адаптера
конкретен, — на compose-site.
{%- if net_services contains "rest" or net_services contains "ssh" %} Сервер
службы ({% if net_services contains "rest" %}`domain::http`{% if net_services contains "ssh" %}, {% endif %}{% endif %}{% if net_services contains "ssh" %}`domain::ssh`{% endif %}) адаптера не получает: стек сети приходит к
узлу рычагом (`STACK: local $crate::net::Stack`), сокет узел открывает сам —
библиотека службы зовётся в `domain` прямо, — а буферы сокета — слоты
`= static` фрагмента{% if net_services contains "mqtt" %}. Из служб через адаптер compose-site
подключается только клиент MQTT: `adapters::net::TcpClient` над стеком, порт
`ports::Dial`{% endif %}.{% endif %}
{%- assign sep = "" %}
{%- capture types %}
{%- if protocol_link == "true" %}{{ sep }}`crate::link::<вид>[::<транспорт>]::Link` (`docs/modules/app.md`){% assign sep = ", " %}{% endif %}
{%- if ota == "true" %}{{ sep }}`adapters::ota::Updater<bsp::FirmwareFlash>`{% assign sep = ", " %}{% endif %}
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
хосте{% endif %}. HAL-объект, который уже реализует нужный трейт, `bsp` отдаёт
псевдонимом типа; своя структура в `bsp` — только обёртка там, где реализации
трейта нет.
{%- assign sep = "" %}
{%- capture wrappers %}
{%- if ota == "true" %}{{ sep }}разделы обновления (`adapters::ota::Partitions`){% assign sep = ", " %}{% endif %}
{%- if has_usb == "true" %}{{ sep }}пару USB (`adapters::usb`){% assign sep = ", " %}{% endif %}
{%- if has_net == "true" %}{{ sep }}пару сети (`adapters::net::Net`){% assign sep = ", " %}{% endif %}
{%- if config_store == "eeprom" %}{{ sep }}EEPROM как `NorFlash`
(`adapters::eeprom::Eeprom`){% assign sep = ", " %}{% endif %}
{%- endcapture %}
{%- if wrappers != "" %} Обратная зависимость есть: `bsp` берёт из `adapters` обёртки,
без которых рычаг не собрать, и строит их сама плата в `Board::new` — {{ wrappers }}{% if has_usb == "true" or has_net == "true" %}; их память ({% if has_usb == "true" %}буферы класса CDC{% if has_net == "true" %}, {% endif %}{% endif %}{% if has_net == "true" %}`StackResources`
стека{% endif %}) — статики `bsp`: кто строит адаптер, тот держит и его память{% endif %}; от `domain` `bsp` не зависит.
{%- else %} От `domain` `bsp` не зависит.
{%- endif %}

### Порт и адаптер — или библиотека прямо в `domain`

Порт в `ports` и его
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
{%- capture net_ports %}{% if protocol_sockets != "0" %}, транспортов протоколов (`ports::FrameLink`){% endif %}{% if net_services contains "mqtt" %}, клиента
   MQTT (`ports::Dial`){% endif %}{% endcapture %}
{%- if net_ports != "" %}{{ sep }}сокеты
   `embassy-net` {{ net_ports | remove_first: ", " }}{% assign sep = ", " %}{% endif %}
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

Порт при этом — только граница логики с адаптером или платой: трейт, у которого
есть реализация вне `domain`. Абстракция, которую и реализует, и зовёт сам
`domain`{% if has_modbus == "true" %} (обработчик кадра узла протокола, `server::Handler`; карта
регистров slave'а, `modbus::Registers`){% endif %}, — его трейт рядом с узлом, а
не порт.

Та же мерка — для датчиков и микросхем, которых в шаблоне ещё нет: объект
драйвера строит `bsp` и отдаёт полем `Board` (рычаг платы), а тип драйвера —
`adapters` или внешний крейт, пока ему хватает трейтов `embedded-hal` (нужен
чип — `bsp`); порт — `ports`, если у логики над датчиком есть что
проверять отдельно или датчиков одного назначения несколько; адаптер «порт над
драйвером» — `adapters`. Светодиоду, у которого проверять нечего, хватает трейта
`embedded-hal`: свой порт ему не нужен.
{%- if graph == "true" %}

### Фрагменты графа: цифры, типы слотов, большие объекты

Цифры узла — тоже в `domain`: `domain::heartbeat::WATCHDOG` фрагмент называет
полным путём `$crate::heartbeat::…`, так что от compose-site он не требует ни
одного объявления. Как они связаны с таймаутом железа и где какое звено
цепочки проверяется — `docs/watchdog.md`.
{%- if ota == "true" %}

**Типы ресурсных слотов** узла OTA — параметры фрагмента: `name: OTA_FRAG<F, R, const PARKED: usize>`,
а compose-site подставляет их в `fragments:` угловыми скобками
(`::domain::OTA_FRAG<::adapters::ota::Updater<bsp::FirmwareFlash>, bsp::reset::Reboot, OTA_CMD_PARKED>`). Слот — `static`, тип ему
нужен, а конкретный тип знает только `cross`; generic-`static` не бывает, и
параметр — единственный способ фрагмента назвать тип, которого он не видит.
Bound'ов у параметров нет намеренно: их держит сигнатура `ota::task`, и rustc
проверяет её, а не копию в DSL. Слот адаптера (`FLASH`) к тому же помечен
`local` — `Updater` не `Send` (`docs/modules/app-graph.md`, «OTA: узел уже в
графе…»); это допущение «один исполнитель» — `docs/flash.md`. Третий параметр,
`PARKED`, — не тип, а число мест ожидания входа `CMD`: его клиентов знает
compose-site, а не фрагмент (там же).
{%- endif %}

**Большой объект — слот `&'static mut T = static`, а не по значению**
(от килобайта): почему и как — `docs/modules/app-graph.md`.
{%- endif %}

### Цена решения «узлы — в `domain`»

`domain` зависит от `supervisor` ради контекста узла
в сигнатуре задачи, а вместе с ним транзитивно от `embassy-executor`,
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
`embedded-storage(-async)` ({% if ota == "true" %}флеш под обновлением{% if config == "true" %}, {% endif %}{% endif %}{% if config == "true" %}носитель настроек{% endif %}),{% endif %}{% if graph == "true" %} трейт крейта
сторожа (`watchdog::HardwareWatchdog`),{% endif %} порты из
`crates-host/ports` там, где стандартного трейта нет{% if has_can == "true" or ota == "true" %} ({% if has_can == "true" %}`CanBus`{% if ota == "true" %}, {% endif %}{% endif %}{% if ota == "true" %}`Reboot`{% endif %}){% endif %}, — и объекты драйверов микросхем платы
({% if external_flash == "true" %}внешняя флеш W25Q; так же лягут{% else %}так лягут{% endif %} датчики и светодиоды; тип драйвера —
`adapters` или внешний крейт{% if external_flash == "true" %}, как `w25q`{% endif %}, а строит объект `bsp`). `Peripherals`,
отдельные пины и группы `assign_resources!` наружу из `bsp` не выходят: их
потребляет сам `Board::new`, собирая рычаги. Приложение работает через трейты
и про регистры не знает.

Протоколов приложения над рычагами `bsp` не собирает: кадрирование транспорта, буферы
сообщений{% if has_net == "true" %}, сокеты протоколов{% if net_services contains "mqtt" %} и клиента MQTT{% endif %}{% endif %}{% if ota == "true" %}, адаптер обновления{% endif %}{% if config == "true" %}, хранилище настроек{% endif %} —
решение проекта, а не свойство платы. Их строит `app` (выше) в своей памяти:
захотел проект другой буфер или порт — правится
`app`{% if protocol_link == "true" or net_services != "" %} ({% if protocol_link == "true" %}`crates-cross/app/src/link/`{% if net_services != "" %}, {% endif %}{% endif %}{% if net_services != "" %}`crates-cross/app/src/net.rs`{% endif %}){% endif %}, плата не меняется. Буфер
заводится там, где решают его размер, — в `app`, — а узел получает ссылку.
{%- if has_usb == "true" or has_net == "true" %} Память, без которой рычаг не собрать
({% if has_usb == "true" %}буферы класса CDC{% if has_net == "true" %}, {% endif %}{% endif %}{% if has_net == "true" %}`StackResources` стека{% endif %}), — наоборот, у платы:
адаптер над ней строит она (выше).{% endif %}
{%- if has_stub == "true" %}

**Выводов шаблон не выбирает — до драйвера в поле `Board` стоит заглушка
(`bsp::stub`).** Какой блок и на каких ножках, знает только плата. Пока драйвер не
подставлен, проект с заглушкой собирается и запускается: приём ждёт вечно (узел
спит, не занимая исполнитель), а передача молча выбрасывает данные. Заглушка —
часть платы, а не приложения: «железо, которого пока нет». Заменить её —
поменять псевдоним в модуле блока на драйвер `embassy-stm32`{% if has_can == "true" %} (у CAN — свой тип
над контроллером, реализующий `ports::CanBus`){% endif %}; рецепт — `docs/modules/bsp-*.md`
модуля блока.
{%- endif %}

`Board` один (`Board::new() -> Board`)
{%- if has_usb == "true" or has_net == "true" %}, и пары, которые нельзя собрать порознь,
собирает сама плата:{% if has_usb == "true" %} USB — `board.<транспорт>: bsp::usb::<транспорт>::Usb {
device, cdc }` (классы `embassy-usb` регистрируются до постройки устройства;
устройство — узлу `USB_*`, поток CDC — транспорту протокола){% endif %}{% if has_usb == "true" and has_net == "true" %} и{% endif %}{% if has_net == "true" %} сеть —
`board.net: bsp::net::Net { runner, stack }` (их рождает один `embassy_net::new`;
фоновая работа — узлу `NET`, стек — сокетам узлов). Стек строит плата, поэтому
и счёт сокетов у неё (`bsp::net::SOCKETS`, `docs/modules/bsp-net.md`).{% if rng_irq != "" %} Затравка стека{% if net_services contains "ssh" %} и бэкенд
`getrandom`{% endif %} над RNG — тоже плата (`bsp::rng`).{% endif %}{% else %}.{% endif %}
{%- else %}.
{%- endif %} {% if graph == "true" %}Граф берёт `Board`
`boot:`-объектом (`boot: board: bsp::Board;`) и делит по полям{% if has_usb == "true" or has_net == "true" %}
({% if has_usb == "true" %}`board.<транспорт>.device`/`.cdc`{% if has_net == "true" %}, {% endif %}{% endif %}{% if has_net == "true" %}`board.net.runner`/`.stack`{% endif %}){% endif %}{% else %}Целиком
`Board` берёт `tasks::spawn_all(spawner, board)`{% endif %}. Отвергнуто: `Option`-поля в
`Board` (тип врёт, ошибка уходит в рантайм){% if has_usb == "true" %}, передача половины пары через
`static` (поток CDC не `Send`){% endif %} и отдельные `Drivers` с парами, собранными в
`app`: тогда `app` строил бы объекты, которые по смыслу — рычаг
платы. Прямое следствие того же правила: `embassy-stm32` нет и в манифесте `app` —
даже драйвер времени (`time-driver-any`) приложение включает фичей
`bsp/time-driver`.
{%- if graph == "true" %}

Сторож (поле `watchdog`) отдаётся **заряженным, но не запущенным** (`wdg::UnarmedBoardWatchdog`), а
запускает его инициализатор блока `watchdog:` графа — `= board.watchdog.arm()`;
почему там и почему забыть нельзя — `docs/watchdog.md`.
{%- endif %}
{%- if ota == "true" %}

{% if graph == "true" %}Кроме сторожа в граф уезжает поле `firmware`{% else %}В задачу узла OTA уезжает поле `firmware`{% endif %} (`bsp::FirmwareFlash` —
`adapters::ota::Partitions`: разделы `DFU` и `STATE` со своими страницами, предел
образа, равный `ACTIVE`, и отпечаток раскладки). {% if graph == "true" %}Compose-site кладёт его во вход узла,
`domain::ota::OtaWiring { flash: Updater::new(board.firmware), reboot: bsp::reset::Reboot, running_version }`,
прямо в `fragments:`: адаптер{% else %}Адаптер{% endif %} обновления (`adapters::ota::Updater`, порт
`FirmwareUpdate`) над ним строит compose-site. Режет разделы плата — по символам
`memory.x` (`bsp::layout`), тем же, что читает bootloader: где они лежат, знает
раскладка, а она — дело платы. Сам `bsp` про `domain` не знает: логика приёма и
применения образа живёт в `domain::ota` и видит только порты; host-тесты подают
адаптеру разделы в памяти, а саму нарезку проверяет сборка `crates-cross`. Образ приходит не по каналу,
а командами узла протокола на вход узла `CMD` ({% if graph == "true" %}`request:` графа, клиент —
`ota_cmd_requester()`{% else %}`static` в `crate::tasks::ota`{% endif %}; `docs/ota.md`). Вместимость раздела адаптер отдаёт через
порт же (`FirmwareUpdate::capacity`). Версия образа — свойство прошивки, а не
платы: её задаёт приложение (`crates-cross/app/src/version.rs`) во входе
узла (`running_version`){% if graph == "true" %} прямо в `fragments:`{% endif %}; ключ подписи — в
bootloader'е (`crates-cross/boot`), который подпись и проверяет.
{%- endif %}

**`Board::new` ничего не настраивает.** Он поднимает HAL (`embassy_stm32::init`
— иначе `Peripherals` взять негде) и собирает объекты. {% if graph == "true" %}Ни сторож не
взводится, ни периферия{% else %}Периферия не{% endif %} конфигурируется «на будущее»:
что делать с объектом, решает приложение, а не `bsp`. Отсюда и имя `new` вместо
прежнего `init`.

Исключения — `ensure_*` для option bytes{% if cache != "" %} и `bsp::cache::enable()`: он включает
кэши чипа сразу за `init` (`docs/clocks.md`, «Кэши»). Это не «на будущее»: с
завода кэши выключены, а знает, что есть у этого ядра, только `bsp`; периферию
ядра (`SCB`) он берёт через `steal()`, не трогая флаг `take()`{% endif %}. Ловушку
деления включает не плата, а `app` (`app/src/fault.rs`, первой строкой `main`).
{%- if external_flash == "true" %}

И внешняя флеш: её драйвер (`bsp::external`) в `Board::new` опознаёт микросхему
— `init` читает JEDEC ID и{% if ext_flash_bus != "spi" %} ставит бит QE, без чего{% else %} без этого{% endif %} драйвер не знает объёма и
не годится ни для одной операции. `init` ждёт микросхему (до 2 с, если она
ещё стирает после сброса), поэтому `Board::new` асинхронный, а зовёт его первая
задача исполнителя (`start` в `main.rs`): ждёт он таймерами `embassy_time`, а
встроенной очереди таймеров нужен waker исполнителя, не `block_on`; не прошёл —
строка в лог, и флеш отказывает, а не паникует (docs/modules/bsp-external.md).
{%- endif %}

**Данные — сведения о чипе, и только они.** `spec: bsp::ChipSpec` — имя,
заводской UID, объём flash, частота `sys` — лежит полем `Board`, хотя порта не
реализует: это то, что приложение сообщает о себе наружу (лог старта, ответ
хосту), а знает это только `bsp`. Тип объявлен в `bsp`, рядом с `Board`, а не
в `ports`: домен его не читает, а порт заводится под то, чем пользуется
логика (раздел «Порт и адаптер — или библиотека прямо в `domain`» выше).
Понадобятся эти сведения домену — тогда тип и переедет в `ports`. По той же
мерке у `Board` нет метода под причину прошлой паники: домену она не нужна,
это диагностика старта, и её печатает `main` напрямую из `panic-persist`.

Целиком `rcc::Clocks` полем не хранится: это тип HAL, и состав его полей
генерируется под чип. `Board::new` печатает его в лог при старте, а `sys`
числом кладёт в `ChipSpec` — их читает target-тест `clocks_match_intent`, см. `docs/clocks.md`.

{%- if ota == "true" %}

**Применение обновления — тоже через порт.** `mark_updated` — метод
`FirmwareUpdate`; запись о подписи (`ports::PendingImage`) кладётся обычным
`write` в запасную страницу `DFU`, отдельного трейта под подпись нет. Порядок
проверок перед вызовом — `domain::ota::update::apply`, и он
host-тестируем. Саму подпись
проверяет bootloader (`domain::ota::boot::screen_update`) — `docs/ota.md`.
{%- endif %}

**Дублировать методы портов собственными нельзя{% if ota == "true" or settings_port == "true" %}.** У {% if ota == "true" %}`adapters::ota::Updater`{% if settings_port == "true" %} и
{% endif %}{% endif %}{% if settings_port == "true" %}`adapters::settings::Settings`{% endif %} {% if settings_port == "true" %}inherent-методы `write`/`read`{% if ota == "true" %}/`mark_booted`{% endif %} были
удалены, а не оставлены{% else %}inherent-метод `mark_booted` был
удалён, а не оставлен{% endif %} рядом с трейтовыми: собственный{% else %}:** собственный{% endif %} метод с тем же именем
перекрывает трейтовый при вызове, и приложение снова начало бы работать с
конкретным типом мимо порта — то есть мимо той самой границы.

**Liquid в корневом workspace — внутри комментариев.** Все члены корневого
workspace (`members` корневого `Cargo.toml`) и сам корневой `Cargo.toml`
компилируются и тестируются в самом репозитории шаблона (`cargo xtask lint`/`test
host`), поэтому их исходник обязан оставаться разбираемым Rust и TOML:

- теги `if`/`endif` — только в строках-комментариях: `//` в `.rs`, `#` в `.toml`;
- плейсхолдер (имя в двойных фигурных скобках) — только внутри строкового
  литерала (`pub const CHIP: &str = "…"` в `crates-host/xtask/src/project.rs`;
  так же `crates-host/xtask/src/pins.rs`{% if protocol == "canopen" %}, `crates-host/canopen/build.rs`{% endif %}) или в
  строке-комментарии, которую генерация превращает в код (приём с
  переменной `newline`: в проекте перевод строки отрывает текст от `//`/`#`).

Условное подключение — то, что зависит от ответа при генерации (невыбранный
протокол уходит вместе со своим крейтом и ссылками на него), — пишется этими двумя приёмами.
В репозитории это обычные комментарии и строки, и проверяются все ветки
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
{%- if ota == "true" %}{{ sep }}`adapters::ota::Updater<bsp::FirmwareFlash>`{% assign sep = ", " %}{% endif %}
{%- if config == "true" %}{{ sep }}`adapters::settings::Settings<bsp::ConfigFlash>`{% assign sep = ", " %}{% endif %}
{%- endcapture %} Рычагу платы его даёт `bsp` ({% if levers != "" %}{{ levers }}{% else %}`pub type X = ...`{% endif %})
{%- if protocol_link == "true" or concrete != "" %}, адаптеру —{% if protocol_link == "true" %} модуль
`app`, который выбирает его тип (`crate::link::<вид>[::<транспорт>]::Link`, `docs/modules/app.md`){% if concrete != "" %},
или{% endif %}{% endif %}{% if concrete != "" %} конкретный тип прямо во входе узла ({{ concrete }} — модуля под одну строку `app`
не заводит){% endif %}{% endif %}. Приложение при этом всё
равно зовёт только методы порта.

