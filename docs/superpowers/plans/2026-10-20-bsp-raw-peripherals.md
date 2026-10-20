# Плата отдаёт периферию — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** `bsp` отдаёт шины, блоки и разделы флеша; адаптеры, их буферы и статики собирает `app` (и `boot`); `bsp` не зависит от `adapters`, `embassy-net`, `embassy-usb`.

**Architecture:** Сборка транспортов (`bsp::link`, стек `bsp::net`, `Builder`/CDC из `bsp::usb`) и адаптеров флеша (`bsp::ota::Updater`, `bsp::settings::Settings`) переезжает в модули `app`; граф берёт их в выражениях `fragments:`, без графа — модуль на задачу. Заглушки периферии и `ErasePage` переезжают из `adapters` в `bsp`, трейт `EepromAccess` — в `ports`.

**Tech Stack:** Rust `no_std`, Embassy (`embassy-stm32` 0.6, `embassy-net` 0.9.1, `embassy-usb` 0.6), Liquid-шаблон cargo-generate, `supervisor_graph!`.

**Spec:** `docs/superpowers/specs/2026-10-20-bsp-raw-peripherals-design.md`

## Global Constraints

- Работа — в worktree `C:\bspwt`, ветка `feat/bsp-raw-peripherals` от `e808636`. Каталог `feat/multi-transport` не трогать.
- В репозитории шаблона `crates-cross/**` содержит Liquid и не собирается: проверка — `template-check`, изолированно: `TMP='C:\tcr' TEMP='C:\tcr' cargo run --release --manifest-path chip-data-gen/Cargo.toml --bin template-check -- <чипы>` (не `C:\tcb` — он у сессии этапа 3).
- Корневой workspace (`domain`, `ports`, `adapters`) собирается целиком: `cargo xtask lint`, `cargo xtask test host` в `C:\bspwt`.
- Liquid в корневом workspace — только внутри комментариев (`// {%- if %}`).
- Зависимости не добавлять, кроме переноса уже существующих между крейтами (`embassy-net-driver`, `embassy-usb-driver`, `embedded-io-async`, `embedded-storage(-async)` у `bsp` — трейты драйверов, версии из `[workspace.dependencies]`). Новая строка в `[workspace.dependencies]` — спросить пользователя.
- `bsp` не зависит от `adapters`, `embassy-net`, `embassy-usb`, `static_cell` (если остаётся нужен только ради статиков адаптеров).
- В `graph.rs` — только конструирование и связывание; статики — в модулях `app`.
- Без графа — модуль на задачу или пул задач в `app`; `main.rs` — исполнитель, `Board`, спавн.
- Задачи embassy не generic: псевдонимы составных типов — в модулях `app`.
- Коммит на задачу, пути — явно; сообщения на русском, с `Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>`.

## Review Focus

- **Проект без транспортов и без OTA/настроек** (`--define interface=none protocol=none ota=false config=false`): `app` не должен тянуть `adapters`, а пустые модули `app/src/{link,net,usb}.rs` должны удаляться хуком — иначе `dead_code`/`unused` под `-D warnings`. Проверка — кейс `template-check` без транспорта в каждой задаче, меняющей хук.
- **Транспорт без протокола** (`uart` без протокола, `can` без протокола, `usb` без протокола): поле `Board` — сырой объект, адаптера в `app` нет; USB без протокола всё равно нуждается в `Builder` и узле `USB_*` — собирать в `app/src/usb.rs` по тому же циклу.
- **Два транспорта одного вида** (`uart`+`uart`, `usb_otg_fs`+`usb_otg_hs`): имена статиков и модулей в `app` по `f[4]`, без коллизий; `clippy::similar_names` уже встречался в `Board::new`.
- **`graph=no`** на каждом затронутом транспорте: те же адаптеры, собранные в модулях задач, а не в `main.rs`.
- **Сеть: число сокетов** — `SOCKETS` переезжает в `app/src/net.rs`; транспорт `tcp`/`udp` без протокола (`e808636` — слот сокета у такого транспорта) обязан сохранить свой слот.

---

### Task 1: Заглушки, `ErasePage`, `mac_from_uid` — в `bsp`; `EepromAccess` — в `ports`

**Files:**
- Create: `crates-cross/bsp/src/stub.rs` — `Serial` (`embedded_io_async::{Read, Write, ReadReady}`), `Can<F>` (`ports::CanBus`), `UsbDriver` (`embassy_usb_driver::Driver`), `NetDevice` (`embassy_net_driver::Driver`), `Flash` (`NorFlash` async и blocking) — тела переносятся из `adapters::{serial,can,usb,net,flash}::Unconnected` без изменений поведения; имена — по таблице ниже.
- Modify: `crates-cross/bsp/src/flash.rs` — `pub struct ErasePage<F, const ERASE_SIZE: usize>` из `adapters::flash` (переезжает целиком, с doc-комментарием).
- Modify: `crates-cross/bsp/src/net.rs` — `pub(crate) fn mac_from_uid(uid: [u8; 12]) -> [u8; 6]`, `pub(crate) fn seed_from_uid(uid: [u8; 12]) -> u64` из `adapters::net`.
- Modify: `crates-host/ports/src/` — новый модуль `eeprom.rs` с `pub trait EepromAccess` (сигнатуры из `adapters::eeprom` без изменений); `adapters::eeprom` использует `ports::EepromAccess`.
- Modify: `crates-cross/bsp/src/{serial,can,usb,net,settings,ota,external}.rs`, `crates-cross/boot/src/{main,signature,external}.rs` — ссылки на новые места.
- Modify: `crates-host/adapters/src/{serial,can,usb,net,flash,eeprom,lib}.rs` — удалить перенесённое и его тесты; `adapters::serial` удаляется целиком, если в нём кроме заглушки ничего нет.
- Modify: `chip-select.rhai` — `file::delete` для `bsp/src/stub.rs` нет (файл нужен всегда, если есть хоть одна заглушка) — внутри файла ветки под Liquid по флагам `has_uart`/`has_can`/`has_usb`/`has_net`/`config_store == "external"`/`ota_store == "external"`.
- Modify: `crates-cross/target-tests/tests/test.rs` — тест `ErasePage`, перенесённый из `adapters::flash` (тот же сценарий, над внутренним флешем раздела `CONFIG` или `DFU`; при отсутствии обоих разделов — не генерируется).

| Было | Стало |
|---|---|
| `adapters::serial::Unconnected` | `bsp::stub::Serial` |
| `adapters::can::Unconnected<F>` | `bsp::stub::Can<F>` |
| `adapters::usb::Unconnected` | `bsp::stub::UsbDriver` |
| `adapters::net::Unconnected` | `bsp::stub::NetDevice` |
| `adapters::flash::Unconnected` | `bsp::stub::Flash` |
| `adapters::flash::ErasePage` | `bsp::flash::ErasePage` |
| `adapters::eeprom::EepromAccess` | `ports::EepromAccess` |

Тесты `adapters`, использующие заглушки как фейк (например `settings` над `flash::Unconnected`), переписать на `test-support`-фейк, который уже есть в `adapters` (`MemFlash`), — не удалять сценарий.

**Interfaces:**
- Produces: `bsp::stub::{Serial, Can<F>, UsbDriver, NetDevice, Flash}`, `bsp::flash::ErasePage<F, N>`, `ports::EepromAccess`, `bsp::net::{mac_from_uid, seed_from_uid}`.

- [ ] **Step 1:** Перенести `EepromAccess` в `ports`, `adapters::eeprom` — на него. Run: `cargo xtask lint && cargo xtask test host` → оба `EXIT=0`.
- [ ] **Step 2:** Перенести заглушки и `ErasePage`, поправить ссылки в `bsp`/`boot`, тесты в `adapters` — на `MemFlash`. Run: `cargo xtask lint && cargo xtask test host` → `EXIT=0`; `grep -rn "Unconnected\|ErasePage" crates-host/adapters/src` → пусто.
- [ ] **Step 3:** Перенести тест `ErasePage` в `target-tests`.
- [ ] **Step 4:** Run `template-check` на `stm32f407ve stm32g474re stm32l073rz stm32h743zi` (F4 — `ErasePage` под `STATE`/`CONFIG`; L0 — EEPROM; H7 — сеть) плюс `--define` с `ota_store=external config_store=external` на одном чипе → все `ok`.
- [ ] **Step 5:** Commit: `refactor(bsp): заглушки периферии и ErasePage — плате, EepromAccess — в ports`.

### Task 2: UART — сырой поток в `Board`, транспорт собирает `app`

**Files:**
- Modify: `crates-cross/bsp/src/serial.rs` — `pub type Serial = crate::stub::Serial;`, `new` без изменений по смыслу.
- Modify: `crates-cross/bsp/src/board.rs` — поле UART с протоколом: `pub <имя>: crate::serial::<имя>::Serial` (как у UART без протокола); `<имя>_buffers` у UART не создаётся.
- Create: `crates-cross/app/src/link.rs` — Liquid-цикл по `links`, ветка `f[0] == "uart"` из `bsp/src/link.rs` целиком: `pub mod <имя> { pub type Link = …; pub fn new(serial: bsp::serial::<имя>::Serial) -> Link; }`, статики `HOST_FRAME`/`HOST_RX`/`HOST_TX`, `HOST_MESSAGE`, `HostBuffers` с `pub fn host_buffers() -> HostBuffers`.
- Modify: `crates-cross/bsp/src/link.rs` — удалить ветки `uart`.
- Modify: `crates-cross/app/src/graph.rs` — для UART-транспорта: `link: crate::link::<имя>::new(board.<имя>)`, буферы — `crate::link::<имя>::host_buffers()`; тип фрагмента — `crate::link::<имя>::Link`.
- Modify: `crates-cross/app/src/main.rs` + новые модули задач (`app/src/tasks/modbus_<имя>.rs`, `host_<имя>.rs` — по правилу «модуль на задачу»): модули задач протоколов из `main.rs` переезжают в `app/src/tasks/`, а UART-задачи берут `crate::link::<имя>::Link`.
- Modify: `crates-cross/app/Cargo.toml` — `adapters` при любом транспорте с протоколом (уже так) ; `static_cell` уже есть.
- Modify: `chip-select.rhai` — `file::delete("crates-cross/app/src/link.rs")` при отсутствии транспорта с протоколом (по тому же условию, что сейчас удаляется `bsp/src/link.rs`, если удаляется; иначе — добавить).

**Interfaces:**
- Consumes: `bsp::stub::Serial` (Task 1).
- Produces: модуль `app::link` с `pub mod <имя> { pub type Link; pub fn new(..) -> Link; pub fn host_buffers() -> HostBuffers }` — Task 4 и 5 добавляют в него ветки `usb`, `tcp`, `udp`; каталог `app/src/tasks/` с модулем на задачу — Task 3–5 добавляют свои.

- [ ] **Step 1:** Перенести UART-ветки, поправить граф и `main.rs` (модули задач — в `app/src/tasks/`).
- [ ] **Step 2:** Run `template-check stm32f407ve` с `--define interface=uart protocol=modbus`, `protocol=protobuf`, `protocol=protobuf graph=false`, `interface=uart protocol=none` и кейс с двумя UART (из 5 кейсов сессии этапа 3, когда придут; до них — `--define` вручную) → все `ok`.
- [ ] **Step 3:** Commit: `refactor(app): транспорт UART собирает app, плата отдаёт поток`.

### Task 3: CAN

**Files:**
- Modify: `crates-cross/bsp/src/can.rs` — `pub type Can = crate::stub::Can<embassy_stm32::can::Frame>;`.
- Modify: `crates-cross/app/src/graph.rs`, `app/src/tasks/canopen_<имя>.rs` — без смысловых изменений, кроме: хранилище — `crate::settings::…` из Task 6 позже; здесь только переезд модуля задачи из `main.rs`.

- [ ] **Step 1:** Перенести задачу CANopen из `main.rs` в `app/src/tasks/`, ссылка на заглушку.
- [ ] **Step 2:** Run `template-check stm32f303re stm32g474re` c `--define interface=can protocol=canopen` и `graph=false` → `ok`.
- [ ] **Step 3:** Commit: `refactor(app): задача CANopen — свой модуль, шина CAN — заглушка платы`.

### Task 4: USB — плата отдаёт драйвер

**Files:**
- Modify: `crates-cross/bsp/src/usb.rs` — на транспорт: `pub type UsbDriver = crate::stub::UsbDriver; pub(crate) fn new(r) -> UsbDriver`. Удалить `VID`/`PID`, `Builder`, `CdcBuffers`, `State`, статики. `bsp` теряет `embassy-usb`, `static_cell` (если больше не нужен).
- Modify: `crates-cross/bsp/src/board.rs` — поле `pub <имя>: crate::usb::<имя>::UsbDriver` (и для USB с протоколом, и без); `<имя>_device` из `Board` уходит.
- Modify: `ChipSpec` — `pub uid_hex: &'static str` (`embassy_stm32::uid::uid_hex()`).
- Create: `crates-cross/app/src/usb.rs` — `VID`/`PID`, на транспорт `pub mod <имя> { pub type Device; pub type Cdc; pub fn new(driver: bsp::usb::<имя>::UsbDriver, serial: &'static str) -> adapters::usb::Usb<…> }` — тело из нынешнего `bsp::usb::<имя>::new` (`config.product = "{{project-name}}"`, `serial_number = serial`).
- Modify: `app/src/link.rs` — ветка `usb` (Rtu/Cobs над `crate::usb::<имя>::Cdc`).
- Modify: `graph.rs`, `app/src/tasks/usb_<имя>.rs` — устройство и поток из `crate::usb::<имя>::new(board.<имя>, board.spec.uid_hex)`; в графе одно выражение даёт и `device`, и `cdc` → разбирать через функцию `crate::usb::<имя>::split(...)`, если граф не допускает `let` — в этом случае `app/src/usb.rs` хранит `Usb` в `StaticCell` и отдаёт части двумя функциями `device()`/`cdc()`. Решить по тому, что принимает `supervisor_graph!` (проверить в `rust-lib`, не выдумывать).
- Modify: `crates-cross/app/Cargo.toml` — `embassy-usb` не нужен напрямую, если типы приходят из `adapters::usb`; иначе — добавить из workspace (перенос, не новая зависимость).
- Modify: `chip-select.rhai` — `file::delete("crates-cross/app/src/usb.rs")` без USB.

- [ ] **Step 1:** Проверить, как фрагмент графа получает два значения из одного конструктора (`rust-lib`, `supervisor_graph!` — входы `= выражение`).
- [ ] **Step 2:** Перенос по списку выше.
- [ ] **Step 3:** Run `template-check` на чипе с двумя USB (`stm32f407ve` — `OTG_FS`+`OTG_HS`), `--define interface=usb protocol=protobuf`, `protocol=modbus`, `protocol=none`, `graph=false`, `ota=true` (готовность USB после подтверждения образа) → `ok`.
- [ ] **Step 4:** Commit: `refactor(app): USB-устройство и CDC собирает app, плата отдаёт драйвер`.

### Task 5: Сеть и RNG

**Files:**
- Modify: `crates-cross/bsp/src/net.rs` — `pub type Device = crate::stub::NetDevice; pub(crate) fn new(uid, r) -> Device` (MAC из `uid` внутри). Удалить `StackResources`, `SOCKETS`, `Stack`, `Net`, `PacketMetadata`. `bsp` теряет `embassy-net`.
- Modify: `crates-cross/bsp/src/rng.rs` — `pub type Rng = embassy_stm32::rng::Rng<'static, RNG>; pub(crate) fn new(r) -> Rng`; поле `Board.rng` (при `rng_irq != ""`); без блока RNG — `ChipSpec.uid` и `seed_from_uid` в `app`.
- Modify: `crates-cross/bsp/src/board.rs` — поля `net: crate::net::Device`, `rng: crate::rng::Rng`.
- Create: `crates-cross/app/src/net.rs` — `SOCKETS` (тот же расчёт `1 + protocol_sockets` и слот транспорта без протокола из `e808636`), `RESOURCES`, `pub fn new(device: bsp::net::Device, seed: u64) -> adapters::net::Net<bsp::net::Device>`.
- Modify: `app/src/link.rs` — ветки `tcp`/`udp` (`PORT`, `SOCKET_*`, `socket()`).
- Modify: `crates-cross/bsp/src/link.rs` — удалить файл; хук — убрать его `file::delete`, если был.
- Modify: `graph.rs`, `app/src/tasks/net.rs` — стек из `crate::net::new(board.net, seed)`; затравка — `board.rng.next_u64()` (или `seed_from_uid(board.spec.uid)` без RNG).
- Modify: `chip-select.rhai` — `file::delete("crates-cross/app/src/net.rs")` без сети; удаление `bsp/src/rng.rs` — по тому же условию, что сейчас.

- [ ] **Step 1:** Перенос по списку выше (та же проблема «одно выражение — два значения» `runner`/`stack`, решение — как в Task 4).
- [ ] **Step 2:** Run `template-check stm32h743zi stm32f407ve` с `--define interface=tcp protocol=modbus`, `interface=udp protocol=protobuf`, `tcp`+`udp` без графа, `tcp` без протокола → `ok`.
- [ ] **Step 3:** Commit: `refactor(app): сетевой стек и сокеты собирает app, плата отдаёт Ethernet и RNG`.

### Task 6: Флеш — разделы из `bsp`, адаптеры в `app`

**Files:**
- Modify: `crates-cross/bsp/src/ota.rs` — `pub struct Partitions { pub dfu: Dfu, pub state: State }`, `pub type Dfu`, `pub type State`, `pub const ACTIVE_LEN: u32`; `pub(crate) fn new(..) -> Partitions`. `Updater` удаляется.
- Modify: `crates-cross/bsp/src/settings.rs` — `pub type ConfigFlash` (раздел `CONFIG` как async `NorFlash`, диапазон `0..capacity` — `BlockingPartition` от `CONFIG_START` длиной `CONFIG_END - CONFIG_START`), `pub(crate) fn new(..) -> ConfigFlash`; EEPROM — `ConfigFlash = adapters`-независимый доступ (`EepromFlash` с `ports::EepromAccess`), обёртку `adapters::eeprom::Eeprom` строит `app`.
- Modify: `Board` — поля `ota: crate::ota::Partitions`, `settings: crate::settings::ConfigFlash`.
- Create: `crates-cross/app/src/ota.rs` — `pub type Updater = adapters::ota::Updater<bsp::ota::Dfu, bsp::ota::State, {{signed}}>; pub fn new(p: bsp::ota::Partitions) -> Updater`; при `graph=no` сюда же переезжает нынешний `mod ota` из `main.rs` (задача, `CMD`, `READY_*`).
- Create: `crates-cross/app/src/settings.rs` — `pub type Settings = adapters::settings::Settings<…>; pub fn new(flash: bsp::settings::ConfigFlash) -> Settings`.
- Modify: `graph.rs`, `main.rs`, `app/src/tasks/canopen_<имя>.rs`, `crates-cross/target-tests/tests/test.rs` (тесты OTA и настроек собирают адаптер сами — `target-tests` получает `adapters` в зависимости), `crates-cross/bsp/Cargo.toml` — убрать `adapters`.
- Modify: `chip-select.rhai` — удаление `app/src/{ota,settings}.rs` по тем же условиям, что `bsp/src/{ota,settings}.rs`.

- [ ] **Step 1:** Перенос по списку выше.
- [ ] **Step 2:** Run: `grep -n "adapters\|embassy-usb\|embassy-net" crates-cross/bsp/Cargo.toml` → пусто.
- [ ] **Step 3:** Run полный `template-check` (все `default_cases()` + 5 кейсов этапа 3, если уже в ветке) → все `ok`.
- [ ] **Step 4:** Commit: `refactor(app): адаптеры OTA и настроек собирает app, плата отдаёт разделы флеша`.

### Task 7: Документация и инварианты

**Files:**
- Modify: `AGENTS.md` — правила «`Board` отдаёт адаптеры…», «Транспорт протокола плата отдаёт готовым…», «`bsp` зависит только от `ports` и `adapters`» → по спецификации (раздел «Правила после работы»); добавить «без графа — модуль на задачу».
- Modify: `docs/architecture.md`, `docs/modules/bsp-{serial,can,usb,net,ota,flash,rng}.md`, `docs/modules/app-*.md` (новые `app-link.md`, `app-net.md`, `app-usb.md` — только если у модулей есть заголовочная документация по образцу `bsp-*.md`), `README.md` (таблица зависимостей), `post-script.rhai` (подсказки «подставьте драйвер в …» — пути остаются в `bsp`, проверить формулировки).
- Документацию нескольких транспортов из задачи 4 этапа 3 пишем здесь же — по итоговой схеме (решение пользователя).

- [ ] **Step 1:** Правка документов.
- [ ] **Step 2:** Run `typos` → `EXIT=0`; `template-check --quick` → нет остаточных `{%`.
- [ ] **Step 3:** Commit: `docs: плата отдаёт периферию — инварианты и модули`.
