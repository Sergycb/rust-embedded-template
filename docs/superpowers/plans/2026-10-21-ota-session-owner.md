# Владелец сессии OTA — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** пока идёт приём образа с одного транспорта, команды приёма с
другого получают `Busy`, а не обрывают и не портят чужую загрузку.

**Architecture:** этап 5 дизайна (раздел 6). На вход `CMD` узла OTA вместо
голой `OtaCommand` приходит `OtaRequest { client, command }`; клиент —
номер транспорта, который раздаёт compose-site в `app`. Узлы протоколов
держат не `Requester`, а `domain::ota::OtaClient` (requester + номер) и
зовут `client.request(command)` как раньше. Автомат OTA запоминает
владельца в состоянии `Receiving` и отвечает `Busy` чужим `Start`/`Chunk`/
`Finish`; владелец освобождается на `Finish`, ошибке и по сроку молчания.

**Tech Stack:** Rust `no_std`, `supervisor` (`supervisor_fragment!`,
`request:`), `fsm::typestate`, `sync-request`, Liquid в `crates-cross/app`.

**Spec:** `docs/superpowers/specs/2026-10-20-multi-transport-design.md`,
раздел 6 «OTA с нескольких транспортов».

## Global Constraints

- Ветка `feat/multi-transport`; коммиты с явными путями; не пушить, не переписывать историю.
- Зависимости не добавлять.
- Клиент — **транспорт**, не удалённый хост (дизайн, раздел 6, «моё решение»).
- `Start` от владельца — как сейчас (перезапуск сессии хостом). `Start`/`Chunk`/`Finish` от другого клиента в `Receiving` — `Err(Rejection::Busy)`, состояние и срок молчания владельца не меняются. `Idle`/`Ready` + `Chunk`/`Finish` — `NoSession` от любого клиента (как сейчас). `Reboot` — от любого клиента (как сейчас). Владелец освобождается: `Finish` (→ `Ready`/`Idle`), ошибка (→ `Idle`), `expired` (→ `Idle`).
- Отказы до стирания остаются до стирания (`AGENTS.md`): чужой `Start` — `Busy` без `prepare`.
- `CMD_BYTES` — одно число на `request:` фрагмента (литерал) и `static` входа без графа; `const`-assert держит размер `OtaRequest`.
- Узлы не падают; задача узла — автомат `#[fsm::typestate]` (`AGENTS.md`).
- Перед правкой Rust — skill `rust-engineering`; `supervisor_fragment!`, `fsm::typestate`, `sync-request` — skill `rust-lib`.
- Проверка: `cargo xtask lint`, `cargo xtask test host` в репозитории шаблона; `chip-data-gen` fmt/clippy/test; `template-check --quick` (все случаи) и точечные полные сборки — исполнитель; полную матрицу — контроллер. `TMP` — свой каталог, не `C:\tcb`.

## Review Focus

1. Хост повторяет `Start` после потери ответа (тот же клиент) — сессия перезапускается, а не `Busy` — тест `the_owner_may_restart_its_session` (Task 1).
2. Владелец замолчал посреди приёма — после срока другой клиент начинает свою сессию — тест `a_silent_owner_releases_the_session` (Task 1).
3. Чужой `Chunk` с тем же смещением, что ждёт владелец, — `Busy`, кусок не записан, следующий кусок владельца принят — тест `a_foreign_chunk_is_busy_and_not_written` (Task 1).
4. Чужой `Start` при `Receiving` не продлевает срок владельца — тест в `a_silent_owner_releases_the_session` шлёт чужой `Start` в середине срока (Task 1).
5. Проект с одним транспортом — поведение прежнее; все существующие тесты `domain::ota` проходят с клиентом 0 (Task 1).

---

### Task 1: `domain::ota` — запрос с клиентом и владелец сессии

**Files:**
- Modify: `crates-host/domain/src/ota.rs` (тип запроса входа `CMD`, `CMD_BYTES` и литерал в `request:`, состояние `Receiving`, действия `start`/`chunk`/`finish`, тестовый стенд `live()` и хелперы шагов)
- Modify: `crates-host/domain/src/lib.rs` (реэкспорт, если `OtaCommand`/`OtaReply` реэкспортируются там же)
- Modify: `crates-host/domain/src/modbus/ota.rs`, `crates-host/domain/src/host/service.rs`, `crates-host/domain/src/canopen/ota.rs`, `crates-host/domain/src/canopen.rs` (слот `OTA_CMD`) — `Requester<…>` → `OtaClient`
- Modify: тесты этих модулей
- Modify: `crates-host/ports/src/ota/command.rs` — doc `Start` («Новый `Start` бросает незаконченный приём» → «того же клиента»)

**Interfaces:**
- Produces:
  - `pub struct ClientId(pub u8)` — `Copy, Clone, Debug, PartialEq, Eq`, `defmt::Format` под фичей, как у соседних типов;
  - `pub struct OtaRequest { pub client: ClientId, pub command: OtaCommand }`;
  - `pub struct OtaClient { … }` — `Copy, Clone`; `pub const fn new(requester: Requester<'static, OtaRequest, OtaReply>, id: ClientId) -> Self`; `pub async fn request(&self, command: OtaCommand) -> Result<OtaReply, RequestDroppedError>` (тот же тип результата, что у `Requester::request`, — узлы протоколов меняют только тип поля);
  - фрагмент: `request: [CMD: $crate::ota::OtaRequest -> $crate::ota::OtaReply; <CMD_BYTES>]`; аксессор графа `ota_cmd_requester()` отдаёт `Requester<…, OtaRequest, OtaReply>`.
- Consumes: ничего из других задач.

- [ ] **Step 1: Тесты** (рядом с существующими в `ota.rs`, на стенде с двумя клиентами `ClientId(0)`, `ClientId(1)`):
  - `a_foreign_start_during_receiving_is_busy` — клиент 0 `Start`, клиент 1 `Start` → `Err(Busy)`; клиент 0 досылает образ и `Finish` → `Ok(Ready)`;
  - `a_foreign_chunk_is_busy_and_not_written` — клиент 1 `Chunk(0, …)` в сессии клиента 0 → `Err(Busy)`; затем `Chunk(0, …)` клиента 0 → `Ok(Received(n))`;
  - `a_foreign_finish_is_busy` — `Finish` клиента 1 в сессии клиента 0 → `Err(Busy)`, сессия продолжается;
  - `the_owner_may_restart_its_session` — клиент 0 `Start`, кусок, снова `Start` → `Ok(Started)`, приём с нуля;
  - `a_silent_owner_releases_the_session` — клиент 0 `Start`; на середине срока клиент 1 `Start` → `Busy`; после срока молчания (от последней команды владельца) клиент 1 `Start` → `Ok(Started)`;
  - `reboot_is_served_for_any_client` — `Reboot` клиента 1 в сессии клиента 0 → `Ok(Rebooting)`.
- [ ] **Step 2:** `cargo xtask test host` — новые тесты FAIL (не компилируются или падают).
- [ ] **Step 3:** реализовать; в таблице автомата — проверка клиента в действиях или защитные переходы, на выбор исполнителя, но таблица остаётся читаемой одним взглядом; срок `until` продлевают только команды владельца.
- [ ] **Step 4:** перевести узлы протоколов на `OtaClient`; их тесты — с `OtaClient::new(…, ClientId(0))`.
- [ ] **Step 5:** `cargo xtask lint`, `cargo xtask test host` — зелёные.
- [ ] **Step 6:** commit `feat(ota): владелец сессии — чужой приём получает Busy`.

---

### Task 2: `app` — номер клиента каждому узлу протокола

**Files:**
- Modify: `crates-cross/app/src/graph.rs` (узлы протоколов в `fragments:`), `crates-cross/app/src/tasks/{modbus,host,canopen,ota}.rs` (без графа), а также любое другое место в `crates-cross/**`, где строится клиент OTA (`grep -rn "requester()" crates-cross`)
- Modify: `docs/ota.md`, `AGENTS.md` (инвариант про вход `CMD`: «каждая команда получает исход…» — дописать владельца сессии), `docs/modules/app-graph.md` — где описан клиент OTA

**Interfaces:**
- Consumes: `domain::ota::{OtaClient, ClientId}` (Task 1).
- Номер клиента — порядковый номер транспорта **с протоколом** в `links`, с 0 (`forloop.index0` по отфильтрованным транспортам либо счётчик через `assign`/`plus` в цикле); один и тот же номер у узла в графе и у задачи без графа.

- [ ] **Step 1:** заменить `ota_cmd_requester()` / `CMD.requester()` у узлов протоколов на `::domain::ota::OtaClient::new(<requester>, ::domain::ota::ClientId(<N>))`.
- [ ] **Step 2:** docs: в `docs/ota.md` — абзац «несколько транспортов»: владелец, `Busy`, освобождение по `Finish`/ошибке/сроку, клиент = транспорт (два хоста на одном транспорте делят сессию).
- [ ] **Step 3:** `chip-data-gen` fmt/clippy/test; `template-check --quick` — все случаи; полные сборки `modbus-uart-uart-tcp`, `protobuf-uart-udp-no-graph`, `canopen-fdcan` (по их define из `default_cases()`).
- [ ] **Step 4:** commit `feat(app): номер клиента OTA у каждого транспорта`.
