# Общая карта регистров Modbus — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** все узлы Modbus проекта (`MODBUS_<ИМЯ>` на каждом транспорте)
читают и пишут **одну** карту регистров: запись по UART видна по TCP.

**Architecture:** этап 4 дизайна (раздел 5). В `domain::modbus` — ручка
`SharedRegisters<R>`: ссылка `&'static Mutex<NoopRawMutex, R>` на карту
проекта, реализующая `ports::Registers` с блокировкой на один запрос.
Каждый узел получает свою копию ручки. Окно OTA (`OtaRegisters`)
оборачивает ручку снаружи, у каждого узла своё. Карту заводит один раз
`app`; узлы графа и задачи без графа получают ручки на неё.

**Tech Stack:** Rust `no_std`, `embassy-sync` (`Mutex`, `NoopRawMutex`),
`static_cell`, `supervisor` (`local` слоты), Liquid в `crates-cross/app`.

**Spec:** `docs/superpowers/specs/2026-10-20-multi-transport-design.md`, раздел 5.

## Global Constraints

- Ветка `feat/multi-transport`; коммиты с явными путями; не пушить, не переписывать историю. В рабочей копии бывают незакоммиченные правки других сессий — их не стейджить и не трогать.
- Новая зависимость — только `embassy-sync = { workspace = true }` в `crates-host/domain/Cargo.toml` (разрешено пользователем 2026-10-21). Других нет.
- Под мьютексом — только карта проекта; `OtaRegisters` — снаружи ручки (дизайн, раздел 5, «моё решение»): запрос OTA, ждущий стирания, не держит карту.
- `NoopRawMutex` — допущение «один исполнитель», то же, что у `FlashMutex` и разделов OTA; ручка дописывается в этот инвариант `AGENTS.md`.
- Узлы не падают; задачи узлов — автоматы `#[fsm::typestate]`; задачи Embassy не generic (`AGENTS.md`).
- Перед правкой Rust — skill `rust-engineering`; `supervisor_fragment!`/слоты — skill `rust-lib`.
- Проверка: `cargo xtask lint`, `cargo xtask test host`; `chip-data-gen` fmt/clippy/test; `template-check --quick` и точечные полные сборки — исполнитель; полная матрица — контроллер.

## Review Focus

1. Запись через одну ручку видна чтению через другую — тест `two_handles_share_one_map` (Task 1).
2. Два запроса с двух ручек одновременно — второй ждёт конца первого, а не паникует и не видит половину записи — тест `concurrent_requests_are_serialized` (Task 1).
3. Окно OTA поверх ручки — команда OTA не берёт мьютекс карты — тест `the_ota_window_does_not_lock_the_map` (Task 1).
4. Проект с одним транспортом Modbus — поведение прежнее (одна ручка на одну карту) — существующие тесты `domain::modbus` и случаи `template-check` (Task 2).
5. Проект без Modbus — `embassy-sync` и ручка не тянут лишнего (условия Liquid в комментариях, как у остального Modbus) — `template-check --quick` (Task 2).

---

### Task 1: `domain::modbus::SharedRegisters`

**Files:**
- Modify: `crates-host/domain/Cargo.toml` (`embassy-sync`, под тем же Liquid-условием, что Modbus, если оно есть у соседних строк)
- Create or modify: `crates-host/domain/src/modbus/shared.rs` (или рядом с `registers.rs` — по устройству модуля), `crates-host/domain/src/modbus.rs` (`mod`/`pub use`)
- Test: в том же модуле

**Interfaces:**
- Produces:
  - `pub type MapMutex<R> = embassy_sync::mutex::Mutex<embassy_sync::blocking_mutex::raw::NoopRawMutex, R>;`
  - `pub struct SharedRegisters<R: 'static> { map: &'static MapMutex<R> }` — `Copy, Clone`, `Debug` (без требования `R: Debug`, если derive его навязывает — вручную);
  - `pub const fn new(map: &'static MapMutex<R>) -> Self`;
  - `impl<R: ports::Registers> ports::Registers for SharedRegisters<R>` — каждый метод: `self.map.lock().await` и вызов того же метода карты.
- Consumes: `ports::Registers`, `domain::modbus::OtaRegisters` (без изменений).

- [ ] **Step 1: Тесты** — карта-фейк с holding-регистрами в памяти:
  - `two_handles_share_one_map` — `write_holding` через ручку A, `read_holding` через ручку B видит записанное;
  - `concurrent_requests_are_serialized` — фейк-метод записи ждёт (`await` на сигнале/таймере между двумя половинами записи); второй запрос с другой ручки, начатый во время ожидания, видит запись целиком;
  - `the_ota_window_does_not_lock_the_map` — `OtaRegisters::new(SharedRegisters::new(&MAP), client)`: пока команда OTA ждёт ответа узла OTA, запрос по другой ручке к карте проходит.
- [ ] **Step 2:** `cargo xtask test host` — FAIL. **Step 3:** реализовать. **Step 4:** `cargo xtask lint`, `cargo xtask test host` — PASS.
- [ ] **Step 5:** commit `feat(modbus): общая карта регистров для нескольких узлов`.

---

### Task 2: `app` — одна карта на все узлы Modbus

**Files:**
- Modify: `crates-cross/app/src/graph.rs` (узлы `MODBUS_<ИМЯ>`), `crates-cross/app/src/tasks/modbus.rs` (без графа); место, где `app` заводит статики (по образцу `link.rs`/`usb.rs` после рефакторинга bsp→app — карта заводится там, где создаётся объект, который её держит)
- Modify: `AGENTS.md` (инвариант `NoopRawMutex`), `docs/modules/app-graph.md` и/или раздел Modbus в `README.md` — как проекту подставить свою карту (тип карты один раз, общий для всех узлов)

**Interfaces:**
- Consumes: `domain::modbus::{SharedRegisters, MapMutex}` (Task 1).
- Тип карты проекта — один псевдоним в `app` (сейчас `domain::modbus::EmptyRegisters`), чтобы проект менял его в одном месте.
- Слот `REGS` фрагмента Modbus — `local` (тип с `&'static` на `!Sync`-мьютекс); узел получает `SharedRegisters` (или `OtaRegisters<SharedRegisters<…>>` с OTA).

- [ ] **Step 1:** завести карту один раз (`StaticCell<MapMutex<…>>` или по прецеденту проекта), раздать ручки узлам в графе и задачам без графа.
- [ ] **Step 2:** docs.
- [ ] **Step 3:** `chip-data-gen` fmt/clippy/test; `template-check --quick` — все случаи; полные сборки `modbus-uart-uart-tcp`, `modbus-tcp-udp`, `modbus-uart` и один `*-no-graph` с Modbus (по define из `default_cases()`).
- [ ] **Step 4:** commit `feat(app): все узлы Modbus — над одной картой`.
