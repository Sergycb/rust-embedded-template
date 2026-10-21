# Оценка RAM в хуке — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** меню транспортов не предлагает молча то, что не поместится в RAM
чипа: такой пункт помечен «не помещается в RAM», `--define` с ним —
ошибка с объяснением; оценка проверяется против фактической RAM каждой
сборки `template-check`.

**Architecture:** этап 7 дизайна (раздел 8). `cargo xtask build` печатает
статическую RAM образа (`.data` + `.bss` + `.uninit` из ELF).
`template-check` собирает её по всем случаям и проверяет, что оценка хука
не ниже факта. Хук считает оценку по таблице измеренных стоимостей: база
(ответы без транспортов) + транспорт × протокол + запас под стек, и
сравнивает с `ram_length` раскладки чипа.

**Tech Stack:** Rust (xtask, `object` — уже зависимость xtask), Rhai (хук),
`chip-data-gen` (`template-check`, тесты каскада).

**Spec:** `docs/superpowers/specs/2026-10-20-multi-transport-design.md`, раздел 8.

## Global Constraints

- Ветка `feat/multi-transport`; коммиты с явными путями; не пушить, не переписывать историю; чужие незакоммиченные правки не трогать.
- Зависимости не добавлять (`object` уже есть в `crates-host/xtask`).
- RAM образа — регион `RAM` из `memory.x` (куда линкуются `.data`/`.bss`), а не сумма всех SRAM чипа. Статическая RAM = сумма размеров секций `.data`, `.bss`, `.uninit` (имена — как в ELF прошивки; секции `DMA_NC`/`.bdma_nc` — не в регионе `RAM`, не считаются).
- Стоимости — **измеренные**, а не выведенные; таблица в хуке с комментарием, откуда числа и как их обновить (раздел в `MAINTAINING.md`).
- Оценка консервативна: для каждого случая `template-check` оценка ≥ факт; иначе `template-check` падает с понятным сообщением (какой случай, оценка, факт).
- Пункт, который с уже отмеченными не помещается, остаётся в меню с пометкой ` — НЕ ПОМЕЩАЕТСЯ в RAM` (как OTA в своём меню); его можно отметить, тогда хук печатает предупреждение. `--define links`/`interface` с не помещающимся — ошибка `links: … не помещается в RAM <чип> (оценка N KiB из M KiB)`.
- Отсев чипов до каскада по `--define links` учитывает RAM.
- Этап 6 (CANopen на нескольких шинах) снят пользователем 2026-10-21 — отметить в дизайне (Task 1).
- Проверка: `cargo xtask lint`/`test host` (xtask — корневой workspace), `chip-data-gen` fmt/clippy/test; полная матрица `template-check` — контроллер.

## Review Focus

1. Чип, у которого раскладка ручная (`write_size` задан или OTA не помещается) — `ram_length` может отсутствовать: оценка не блокирует, меню без пометок — тест `manual_layout_skips_the_ram_estimate` (Task 3).
2. Чип с маленькой RAM (L0/F0, 8 KiB) и `ethernet`/`usb` — пункт помечен — тест `a_transport_that_does_not_fit_is_marked` (Task 3).
3. Отметка пункта, после которой уже отмеченные перестают помещаться, — пометки пересчитываются при каждой перерисовке — тест `marks_are_recomputed_after_each_toggle` (Task 3).
4. Сборка, у которой нет одной из секций (`.uninit` нет у проекта без `panic-persist`) — xtask считает 0, а не падает — тест в xtask `missing_sections_count_as_zero` (Task 2).
5. Оценка ниже факта на каком-то чипе — `template-check` падает с именем случая (Task 2, проверка контроллером на полной матрице).

---

### Task 1: Дизайн — этап 6 снят

**Files:** `docs/superpowers/specs/2026-10-20-multi-transport-design.md` (цели 8, раздел 5a, «Этапы»).

- [ ] **Step 1:** пометить цель 8 и раздел 5a как снятые 2026-10-21 (в меню CAN выбирается один раз; хук отвергает второй CANopen и в `--define`), этап 6 в списке этапов — снят. Commit `docs(spec): CANopen на нескольких шинах снят`.

---

### Task 2: Статическая RAM — в `xtask build` и в `template-check`

**Files:**
- Modify: `crates-host/xtask/src/size.rs` (новая функция отчёта рядом с `report_image`), `crates-host/xtask/src/firmware.rs` (вызов), `crates-host/xtask/src/memory.rs` (регион `RAM` — если ещё не читается)
- Modify: `chip-data-gen/src/bin/template-check.rs` (собрать строку `ram:` из вывода `cargo xtask build` каждого случая; сводная таблица в конце прогона; проверка «оценка ≥ факт» включается в Task 3, здесь только сбор и печать)

**Interfaces:**
- Produces: `pub fn static_ram(elf: &[u8]) -> Result<u64, anyhow::Error>` (сумма `.data` + `.bss` + `.uninit`, отсутствующая секция — 0); строка вывода `ram: <N> байт статической RAM из <M> байт RAM (<p>%)` — формат фиксирован, его читает `template-check`.
- Produces: в `template-check` — `fn parse_static_ram(build_output: &str) -> Option<u64>` и таблица `случай → байт` в конце прогона.

- [ ] **Step 1: Тесты** — xtask: `static_ram_sums_data_bss_and_uninit` (ELF-фикстура или собранный в тесте минимальный ELF через `object::write`, если это уже практика в xtask; иначе — функция над списком `(имя, размер)` и тест на неё), `missing_sections_count_as_zero`; template-check: `parse_static_ram` на образце вывода.
- [ ] **Step 2:** FAIL → реализовать → PASS (`cargo xtask lint`, `cargo xtask test host`, `chip-data-gen` test).
- [ ] **Step 3:** полная сборка одного случая (`stm32f407ve`) — строка `ram:` в выводе, таблица в конце.
- [ ] **Step 4:** commit `feat(xtask): статическая RAM образа в отчёте сборки`.

---

### Task 3: Таблица стоимостей и оценка в хуке

**Files:**
- Modify: `chip-select.rhai` (таблица `RAM_COST`, функция оценки, пометки в меню `transports:`, ошибки `--define`, отсев до каскада)
- Modify: `chip-data-gen/tests/cascade.rs`
- Modify: `chip-data-gen/src/bin/template-check.rs` (проверка «оценка ≥ факт»: оценку хук отдаёт переменной `ram_estimate` (строка, байты), `template-check` читает её из сгенерированного проекта или из вывода хука)
- Modify: `MAINTAINING.md` (как измерить и обновить таблицу)

**Interfaces:**
- Consumes: таблица фактов Task 2.
- Produces: переменная хука `ram_estimate`; таблица `RAM_COST` — база по `(ota, config, graph)` и стоимость транспорта по `(вид, протокол)`, сетевой стек — один раз на ethernet; `STACK_RESERVE` (байт) — с обоснованием из измерений.

- [ ] **Step 1: Измерения.** Контроллер даёт таблицу фактов полной матрицы (Task 2). Дополнительно исполнитель собирает пары «с транспортом / без» на одном чипе (`stm32h743zg`, `stm32f407ve`, `stm32g474re`) для каждого вида × протокола, где сборки ещё нет, — точечными полными сборками; разности — стоимости. База — максимум по чипам случаев без транспорта. Числа и метод — в отчёт.
- [ ] **Step 2: Тесты** (Review Focus 1–3) + `the_estimate_is_exported` (`ram_estimate` — число, растёт с транспортом) + `define_links_that_do_not_fit_are_rejected`.
- [ ] **Step 3:** FAIL → реализовать → PASS.
- [ ] **Step 4:** `template-check`: проверка «оценка ≥ факт» по каждому случаю; `--quick` — все случаи; полная сборка 3–4 случаев.
- [ ] **Step 5:** commit `feat(hook): оценка RAM — меню и --define не предлагают невлезающее`.
