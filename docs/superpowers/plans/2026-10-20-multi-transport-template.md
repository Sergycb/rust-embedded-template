# Несколько транспортов: шаблон по списку — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** сгенерированный проект содержит **каждый** выбранный транспорт:
своё поле `Board`, свою группу в `resources.rs`, свой узел протокола (или
заглушку без протокола), а однотранспортные проекты собираются, как
раньше (меняются только имена).

**Architecture:** этап 3 дизайна. Хук считает производные переменные и
`file::delete` по **множеству** транспортов и отдаёт флаги наличия
(`has_modbus`, `modbus_rtu`, …). Файлы корневого workspace переходят с
`interface ==`/`protocol ==` на флаги. В `crates-cross` всё «на транспорт»
генерируется циклом Liquid по `links`: в каждом модуле драйвера и в
`link.rs` — подмодуль на экземпляр с именем транспорта
(`bsp::serial::usart1`, `bsp::link::tcp`), поле `Board` с тем же именем,
узел графа `<ПРОТОКОЛ>_<ИМЯ>`.

**Tech Stack:** Rhai (хук), Liquid (cargo-generate 0.23), Rust `no_std`
(Embassy, `supervisor`), тесты хука — `chip-data-gen/tests/cascade.rs`,
сборка — `template-check`.

**Spec:** `docs/superpowers/specs/2026-10-20-multi-transport-design.md`
(разделы 3, 4, 5 — кроме общей карты регистров; этап 3). Предыдущий план:
`docs/superpowers/plans/2026-10-20-multi-transport-hook.md`.

## Global Constraints

- Ветка `feat/multi-transport`; коммиты с явными путями, без `git add -A`; не пушить; в `master` не вливать.
- Зависимости не добавлять.
- `links` — `вид:метка:блок:протокол:имя` через запятую (`uart:usart:USART1:modbus:usart1,tcp:eth:ETH:modbus:tcp`); пустая строка — транспортов нет. Цикл: `{%- assign xs = links | split: "," -%}{%- for l in xs %}{%- assign f = l | split: ":" %}…{% endfor %}`; `f[0]` вид, `f[1]` метка, `f[2]` блок, `f[3]` протокол, `f[4]` имя (проверено спайком: пустой `links` даёт ноль итераций).
- Имя транспорта `f[4]` — имя поля `Board`, подмодуля в `bsp` и суффикс узла: `MODBUS_USART1`, `HOST_TCP`, `CANOPEN_FDCAN1`, `USB_USB_OTG_FS` (фоновый узел USB на экземпляр); `NET` — один на `tcp`+`udp`.
- Имя группы `resources.rs` — `f[4]`, тип группы — CamelCase от `f[4]` по `_` (`usb_otg_hs` → `UsbOtgHsResources`), через `capture` в цикле.
- Протокол транспорта в шаблоне берётся из `f[3]`, наличие протокола — из флагов `has_modbus`/`has_protobuf`/`has_canopen`, а не из скаляра `protocol` (снятие правила «один протокол» не должно требовать прохода по шаблонам).
- Liquid в корневом workspace — только в комментариях (`AGENTS.md`).
- Задачи Embassy не generic: каждому типу, уезжающему в задачу, — псевдоним в `bsp` (`bsp::link::usart1::Link`).
- Узлы не падают; `restart: Never` у всех (`AGENTS.md`).
- CANopen — не больше одного транспорта до этапа 6 (статики `zencan`).
- После каждой задачи: `cargo fmt/clippy/test` для `chip-data-gen` (`MAINTAINING.md:603-612`) и полный `template-check` (без `--quick`), 44 случая + новые; запуск с `TMP='C:\tcb' TEMP='C:\tcb'`.
- Задачи 1–2 не меняют сгенерированные однотранспортные проекты: проверка — генерация 44 случаев старым и новым шаблоном (`--quick --keep` в двух каталогах) и `diff -r --strip-trailing-cr` без отличий.

## Review Focus

1. Проект без транспорта (`links` пуст) — ни одного пустого подмодуля, поля или узла; `resources.rs` без пустых групп — проверяется существующими случаями без интерфейса в `template-check` (Task 3).
2. Два транспорта одного вида (`usart1`, `usart2`) — уникальные имена статиков, типов, задач; ловит новый случай `modbus-uart-uart-tcp` (Task 4).
3. `tcp` + `udp` на одном стеке — `SOCKETS` = 1 + число сокетов протоколов, один узел `NET`, разные порты не требуются (разные транспорты IP) — случай `modbus-tcp-udp` (Task 4).
4. Транспорт без протокола рядом с протоколом (`uart:modbus` + `can`) — `can` получает заглушку и поле, `adapters::can`/`ports::can` остаются — случай `modbus-uart-can` (Task 4).
5. Без графа — задача на каждый транспорт в `main.rs`, имена задач уникальны — случай `*-no-graph` с двумя транспортами (Task 4).

---

### Task 0: Старая ошибка сборки при `ota=external` без настроек

**Files:**
- Modify: `crates-host/adapters/src/flash.rs:291`

Тест `settings_over_an_unconnected_flash_refuse_without_panicking` открыт условием `external_flash == "true"`, а `crate::settings` есть только при `settings_port == "true"` (`adapters/src/lib.rs:25`). При `ota=external` и `config=no` (случай `stm32l073rz` `ota-external`) тест не компилируется.

- [ ] **Step 1:** условие строки 291 — `external_flash == "true" and settings_port == "true"`, закрывающий `endif` (`:303`) без изменений.
- [ ] **Step 2:** `TMP='C:\tcb' TEMP='C:\tcb' cargo run --release --manifest-path chip-data-gen/Cargo.toml --bin template-check -- stm32l073rz --define ota=external` — проходит; затем полный прогон без аргументов — 44/44.
- [ ] **Step 3:** commit `fix(adapters): тест заглушки внешней флеши — только при хранилище настроек`.

---

### Task 1: Производные переменные хука — по множеству транспортов

**Files:**
- Modify: `chip-select.rhai` (производные `:1642-1668` по номерам до этапа 2 — искать по именам переменных; `file::delete` после них; запрет второго CANopen в `parse_links` и в меню)
- Modify: `chip-data-gen/tests/cascade.rs`

**Interfaces:**
- Consumes: список транспортов хука (запись `#{ kind, tag, block, protocol }`, `links`, `has_uart`…`has_net`).
- Produces (новые переменные, `"true"`/`"false"`):
  - `has_modbus`, `has_protobuf`, `has_canopen` — есть транспорт с этим протоколом;
  - `modbus_rtu` — Modbus поверх хоть одного `uart`/`usb`; `modbus_tcp`; `modbus_udp`; `modbus_mbap` = `modbus_tcp` или `modbus_udp`;
  - `cobs_used` — protobuf поверх хоть одного потока (`uart`/`usb`/`tcp`);
  - `protocol_sockets` — число (строкой) транспортов `tcp`/`udp` с протоколом.
- Produces (прежние, теперь по множеству): `link_port` (= `has_protobuf` или есть `can` или `udp`), `stream_io` (есть `uart`/`usb`/`tcp`), `crc_used` (= `modbus_rtu` или `cobs_used`), `framing_tests` (= `has_modbus` или `cobs_used`), `ports_async` (прежнее условие с «есть `can`/`udp`»), `protocol_socket` (= `protocol_sockets` > 0), `protocol_link` (= `has_modbus` или `has_protobuf`), `has_resources` (есть транспорт или прежние условия), `settings_port` (`config` или `has_canopen`).
- `file::delete` — по тем же флагам: `serial.rs`/`bsp-serial.md`/`adapters/serial.rs` — нет `uart`; `can.rs`/`bsp-can.md`/`ports/can.rs`/`adapters/can.rs` — нет `can`; `rtu.rs` — нет `modbus_rtu`; `modbus/tcp.rs`, `modbus/udp.rs`, `mbap.rs` — по `modbus_tcp`/`modbus_udp`/`modbus_mbap`; `net/tcp.rs`, `net/udp.rs` — нет `tcp`/`udp`; `cobs.rs` — нет `cobs_used`; USB, NET, RNG — нет `usb`, нет `has_net`; стеки протоколов — по `has_*`. Скаляры `interface`, `protocol`, `iface_periph`, `can_kind`, `usb_kind` остаются (их читают файлы до Task 3 и `post-script.rhai`).
- Второй транспорт с `canopen` — ошибка `links: CANopen пока только на одной шине CAN` (`--define`); в меню второй и далее `can` в проекте с CANopen получает только `none`.

- [ ] **Step 1: Тесты** — `derived_flags_follow_every_link` (f407ve, `links=uart:modbus,tcp:modbus,can`: `modbus_rtu`, `modbus_tcp`, `modbus_mbap`, `has_can`, `link_port`, `stream_io` — `"true"`; `modbus_udp`, `cobs_used`, `has_protobuf` — `"false"`; `protocol_sockets == "1"`; `serial.rs`, `can.rs`, `rtu.rs`, `modbus/tcp.rs` не в `deleted`, `modbus/udp.rs` и `cobs.rs` — в `deleted`); `protobuf_over_udp_and_a_stream_keeps_cobs` (`links=udp:protobuf,uart:protobuf`: `cobs_used == "true"`, `cobs.rs` не удалён); `a_second_canopen_bus_is_rejected` (f407ve, `links=can:canopen,can:canopen` — ошибка с текстом выше); `the_second_can_in_a_canopen_project_has_no_protocol_menu` (интервью: `can`/`canopen`, `more_links=yes`, `can` — второго `protocol:` нет, в `links` `…:none:can2`).
- [ ] **Step 2:** тесты FAIL. **Step 3:** реализовать. **Step 4:** `cargo test --manifest-path chip-data-gen/Cargo.toml --release` — PASS.
- [ ] **Step 5:** генерация 44 случаев до и после — без отличий (Global Constraints). Полный `template-check` — 44/44.
- [ ] **Step 6:** commit `feat(hook): производные переменные по всем транспортам`.

---

### Task 2: Корневой workspace — флаги вместо `interface ==`/`protocol ==`

**Files (только Liquid-условия в комментариях):** `Cargo.toml`, `deny.toml`, `crates-host/ports/{Cargo.toml,src/lib.rs}`, `crates-host/adapters/{Cargo.toml,src/lib.rs,src/modbus.rs,src/net.rs,src/settings.rs}`, `crates-host/domain/{Cargo.toml,src/lib.rs,src/ota.rs}`, `.claude/rules/domain.md`.

**Interfaces:**
- Consumes: флаги Task 1 и `has_uart`…`has_net`.
- Правило замены: `interface == "uart"` → `has_uart == "true"` (так же `can`, `usb`, `tcp`, `udp`); `interface == "tcp" or interface == "udp"` → `has_net == "true"`; `protocol == "modbus"` → `has_modbus == "true"` (так же protobuf, canopen); `protocol != "none"` → `has_modbus == "true" or has_protobuf == "true" or has_canopen == "true"`; пары «протокол + вид» → `modbus_rtu`/`modbus_tcp`/`modbus_udp`/`modbus_mbap`/`cobs_used`. Условие, не укладывающееся в правило, — новый флаг в хуке (с тестом в `cascade.rs`), а не составное выражение в шаблоне.

- [ ] **Step 1:** заменить; `grep -rnE 'interface ==|protocol ==|protocol !=|interface !=' <файлы задачи>` — пусто.
- [ ] **Step 2:** `cargo xtask lint` и `cargo xtask test host` в репозитории шаблона — зелёные (крейты корневого workspace компилируются целиком).
- [ ] **Step 3:** генерация 44 случаев до и после — без отличий; полный `template-check` — 44/44.
- [ ] **Step 4:** commit `refactor(template): корневой workspace смотрит на флаги транспортов`.

---

### Task 3: `crates-cross` — транспорт на экземпляр

**Files:**
- Modify: `crates-cross/bsp/src/{resources.rs,board.rs,link.rs,serial.rs,can.rs,usb.rs,net.rs,lib.rs}`, `crates-cross/bsp/Cargo.toml`, `crates-cross/Cargo.toml`, `crates-cross/app/{Cargo.toml,src/graph.rs,src/main.rs}`
- Modify: `chip-data-gen/tests/cascade.rs` (`template_variables()` пропускает переменные циклов: имена, объявленные `{% for X in %}` и `{% assign X = %}` в том же файле)

**Interfaces:**
- Consumes: `links`, флаги Task 1.
- Produces (сгенерированный код):
  - `resources.rs`: группа на транспорт с блоком, кроме `tcp`/`udp`: `usart1: Usart1Resources { usart: USART1 }`, `fdcan1: Fdcan1Resources { can: FDCAN1 }`, `usb_otg_fs: UsbOtgFsResources { usb: USB_OTG_FS }`; одна `eth: EthResources { eth: ETH }` при `has_net`; `rng` — как сейчас при `has_net`.
  - `serial.rs`/`can.rs`/`usb.rs`: подмодуль на экземпляр `pub mod <имя> { pub type …; pub(crate) fn new(_resources: crate::resources::<Camel>Resources) -> … }`; в `usb.rs` статики `BUFFERS`/`CDC_STATE` и псевдонимы `UsbDriver`/`Usb`/`Cdc`/`Device` — внутри подмодуля (у реального драйвера тип зависит от блока). Общие константы (`VID`, `PID`) — вне подмодулей.
  - `net.rs`: `SOCKETS = 1 + protocol_sockets`; `PacketMetadata` — при `has_udp`.
  - `link.rs`: подмодуль на транспорт с протоколом `modbus`/`protobuf`: `pub type Link`, `pub(crate) type Source`, статики буферов/сокета, `pub(crate) fn new(source: Source) -> Link`; у protobuf — `HostBuffers` и `pub(crate) fn host_buffers() -> HostBuffers` в подмодуле (тип `HostBuffers` — один, вне подмодулей). Порт: `tcp` — 502 (Modbus) / 5000 (protobuf), `udp` — те же.
  - `board.rs`: поле `pub <имя>: …` на транспорт: с `modbus`/`protobuf` — `crate::link::<имя>::Link` (+ `pub <имя>_buffers: crate::link::HostBuffers` у protobuf); с `canopen` и без протокола — объект заглушки (`crate::can::<имя>::Can`, `crate::serial::<имя>::Serial`); `usb` — ещё `pub <имя>_device: crate::usb::<имя>::Device` при протоколе или `pub <имя>: crate::usb::<имя>::Usb` без него; `net` — одно поле при `has_net`.
  - `graph.rs` (`fragments:`): на транспорт с протоколом — узел `<ПРОТОКОЛ>_<ИМЯ ЗАГЛАВНЫМИ>` (`MODBUS_SLAVE_FRAG<bsp::link::usart1::Link, …> as MODBUS_USART1`, `HOST_FRAG<…> as HOST_TCP`, `CANOPEN_FRAG<bsp::can::fdcan1::Can, …> as CANOPEN_FDCAN1`), каждый с `ota_cmd_requester()` при OTA; на `usb` — `USB_FRAG<bsp::usb::<имя>::Device> as USB_<ИМЯ>`; `NET` — один.
  - `main.rs` (без графа): задача на транспорт — модуль `<протокол>_<имя>` (`mod modbus_usart1 { #[embassy_executor::task] pub async fn task(…) }`), `usb_<имя>`, `net`; спавн каждой; `index: 0` у всех контекстов.

- [ ] **Step 1:** `template_variables()` — тест `loop_variables_are_not_hook_variables` (временный файл с `{% for l in xs %}{{ l }}{% assign f = l | split: ":" %}{{ f[0] }}` — `l`, `f`, `xs` не возвращаются); FAIL → реализовать → PASS.
- [ ] **Step 2:** переписать файлы `bsp` и `app`; скаляры `interface`, `protocol`, `iface_periph`, `can_kind`, `usb_kind`, `protocol_link`, `protocol_socket` в `crates-cross/**` больше не читаются (`grep` — пусто), `can_kind`/`usb_kind` берутся из `f[1]`.
- [ ] **Step 3:** полный `template-check` — 44/44 (однотранспортные проекты собираются с новыми именами).
- [ ] **Step 4:** commit `feat(template): транспорт на экземпляр — поле, группа, узел`.

---

### Task 4: Новые случаи `template-check` и документация

**Files:**
- Modify: `chip-data-gen/src/bin/template-check.rs` (`default_cases()`, счётчик в doc-комментарии модуля)
- Modify: `README.md` (раздел «Протокол связи и интерфейс»), `AGENTS.md` (`Board::link` → поля по транспортам; правило «транспорт протокола плата отдаёт готовым»), `MAINTAINING.md` (матрица случаев, скаляры vs `links`), `docs/resources.md`, `docs/modules/bsp-{serial,can,usb,net}.md`, `docs/architecture.md`, `docs/modules/app-graph.md`, `cargo-generate.toml` (оговорку «пока основной транспорт» снять)

**Новые случаи** (`--define`, чип):
- `modbus-uart-uart-tcp` — `stm32h743zg`, `links=uart:modbus,uart:modbus,tcp:modbus`;
- `protobuf-usb-usb` — `stm32f407ve`, `links=usb:protobuf,usb:protobuf`;
- `modbus-tcp-udp` — `stm32f407ve`, `links=tcp:modbus,udp:modbus`;
- `modbus-uart-can` — `stm32f407ve`, `links=uart:modbus,can`;
- `protobuf-uart-udp-no-graph` — `stm32f407ve`, `links=uart:protobuf,udp:protobuf`, `graph=no`.

- [ ] **Step 1:** добавить случаи; полный `template-check` — 49/49.
- [ ] **Step 2:** документация; `typos` — без ошибок.
- [ ] **Step 3:** commit `test(template): случаи с несколькими транспортами; docs`.
