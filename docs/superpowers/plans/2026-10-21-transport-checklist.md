# Список транспортов с отметками — план реализации

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** вместо цикла «транспорт → протокол → добавить ещё?» пользователь
отмечает транспорты в одном списке с квадратиками, жмёт «готово», и для
каждого отмеченного отвечает на привычный вопрос о протоколе. Ничего не
отмечено — проекта без транспортов.

**Architecture:** только хук `chip-select.rhai` и его тесты. Хуки
`cargo-generate` (до 0.25.0 включительно) умеют лишь одиночный выбор
(`variable::prompt(текст, умолчание, варианты)`), поэтому список с
отметками собран из него: меню перерисовывается после каждой отметки.
Список строится после выбора чипа — из его выведенных блоков. Модель
транспортов, `links`, флаги и всё, что после вопросов, не меняется.

**Tech Stack:** Rhai (хук cargo-generate), тесты хука — `chip-data-gen/tests/cascade.rs`.

**Spec:** `docs/superpowers/specs/2026-10-20-multi-transport-design.md`,
раздел 2 (порядок вопросов) — этот план заменяет в нём цикл с `more_links`.

## Global Constraints

- Ветка `feat/multi-transport`; коммиты с явными путями; не пушить, не переписывать историю.
- Зависимости и версию `cargo-generate` не менять.
- Меню `transports:` — после вопроса о графе, там же, где сейчас первый `interface:`. Первый пункт — `готово`, он же умолчание. Дальше — по пункту на каждый доступный блок, в порядке видов `uart`, `can`, `tcp`, `udp`, `usb`, внутри вида — в порядке `INSTANCES` (как `free_blocks`); у `tcp` и `udp` — по одному пункту на ETH.
- Текст пункта: `[x] ` или `[ ] `, затем значение `<вид>:<блок>` (`uart:USART2`, `tcp:ETH`), затем ` — ` и `INTERFACE_TEXT[вид]`. Выбор пункта переключает отметку и показывает меню заново; выбор `готово` заканчивает список.
- Отмеченные транспорты идут в список транспортов хука в порядке пунктов меню. Блок — тот, что в пункте (*решение контроллера:* пользователь видит и выбирает физический блок; раньше блок назначался по порядку, но список с квадратиками без имён блоков не выразил бы «второй UART»).
- После списка — вопрос `protocol:` на каждый отмеченный транспорт по нынешним правилам (один протокол в проекте; уже выбранный протокол и `none`; вид, который протокол не несёт, — без вопроса; CANopen — на одной шине). `modbus_master` — как сейчас.
- Вопроса `more_links` больше нет.
- `--define links=…`, `--define interface/protocol`, их ошибки и отсев чипов до каскада — без изменений.
- Без терминала: `готово` по умолчанию → проект без транспортов (как сейчас).
- Переменные шаблона (`links`, `has_*`, производные, скаляры основного транспорта) — без изменений: сгенерированные проекты для всех случаев `template-check` не меняются.

## Review Focus

1. Отметить и снять тот же пункт — транспорт не попадает в проект — тест `toggling_twice_leaves_the_transport_out`.
2. `готово` без отметок — без транспортов, вопросов о протоколе нет — тест `done_without_marks_gives_no_transports`.
3. Чип с одним блоком вида и `tcp`+`udp` на одном ETH — оба пункта сети есть и отмечаются вместе — тест `tcp_and_udp_are_separate_items_on_one_eth`.
4. Отмечены `can` и `uart`, протокол Modbus выбран на `uart` — вопрос о протоколе у `can` не задаётся, основной транспорт — `uart` — тест `the_protocol_menu_follows_the_marked_order`.
5. Без терминала — меню `transports:` показано один раз, проект без транспортов — тест `no_terminal_leaves_the_list_empty`.

---

### Task 1: Меню с отметками в хуке

**Files:**
- Modify: `chip-select.rhai` (цикл вопросов о транспортах, ~`:1080-1190`; `INTERFACE_TEXT`; вопрос `more_links` удаляется)
- Modify: `chip-data-gen/tests/cascade.rs` (мок `variable::prompt` — выбор пункта по значению после префикса `[x] `/`[ ] `; тесты, которые сейчас отвечают на `interface:`/`more_links`, — на новое меню)
- Modify: `cargo-generate.toml` (комментарий о вопросах), `MAINTAINING.md` (порядок вопросов), `README.md` — только если там описан вопрос «добавить ещё один транспорт» (`grep -n more_links`)

**Interfaces:**
- Produces: меню с id `transports:` (`"transports: отметьте транспорты (Enter — отметить или снять)"`), пункты по Global Constraints; список транспортов хука — тот же формат записи `#{ kind, tag, block, protocol }`.
- Consumes: `free_blocks`, `INTERFACE_TEXT`, правила протокола — существующие.

- [ ] **Step 1: Мок.** В `run_interview` ответ `("transports", "uart:USART2")` выбирает пункт, у которого после префикса `[x] `/`[ ] ` идёт `uart:USART2 `; ответ `("transports", "готово")` — пункт `готово`. Ответы одного id расходуются по порядку (как сейчас).
- [ ] **Step 2: Тесты** (`cascade.rs`, чипы — по `chip-data/instances.rhai`): пять из Review Focus и
  - `marked_transports_keep_the_menu_order` — `f407ve`: отметить `tcp:ETH`, затем `uart:USART1`, `готово` → `links` начинается с `uart:usart:USART1:`, затем `tcp:eth:ETH:`;
  - `marks_are_shown_in_the_redrawn_menu` — после отметки `uart:USART1` второе показанное меню содержит пункт, начинающийся с `[x] uart:USART1`;
  - `more_links_is_gone` — ни в одном прогоне нет вопроса с id `more_links`;
  - прежние тесты интервью (`the_menu_offers_a_kind_only_while_blocks_remain`, `once_a_protocol_is_chosen_only_it_and_none_are_offered`, `a_kind_the_protocol_cannot_carry_gets_no_protocol_question`, `the_second_can_in_a_canopen_project_has_no_protocol_menu`, `no_terminal_adds_no_extra_links` и т. п.) — переписать на новое меню с тем же смыслом или удалить, если смысл исчез (сказать в отчёте, какие).
- [ ] **Step 3:** `cargo test --manifest-path chip-data-gen/Cargo.toml --release --test cascade` — новые FAIL.
- [ ] **Step 4:** реализовать.
- [ ] **Step 5:** `cargo fmt/clippy/test` для `chip-data-gen`; `template-check --quick` — все случаи (свой `TMP`, не `C:\tcb`); сгенерированные проекты не должны меняться — `--quick --keep` до и после, `diff -r --strip-trailing-cr` пустой (как в прежних планах).
- [ ] **Step 6:** commit `feat(hook): транспорты — один список с отметками`.
