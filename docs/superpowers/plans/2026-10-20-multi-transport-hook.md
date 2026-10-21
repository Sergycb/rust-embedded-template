# Несколько транспортов: данные о чипах и хук — план реализации

> 2026-10-21: цикл `more_links`, описанный ниже, позже заменён меню-списком (см. `2026-10-21-transport-checklist.md`).

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** хук генерации умеет несколько транспортов (вопрос «Добавить ещё
один транспорт?», `--define links=…`, блоки по числу физических у чипа), а
однотранспортные проекты генерируются ровно как сейчас.

**Architecture:** этапы 1–2 дизайна. `chip-data-gen` пишет в
`chip-data/instances.rhai` список всех выведенных блоков каждой метки, с
проверкой выводов нужных сигналов. `chip-select.rhai` хранит ответы списком
транспортов, отдаёт его Liquid переменной `links` и флагами `has_*`, а
прежние скаляры (`interface`, `protocol`, `iface_periph`, `can_kind`,
`usb_kind`) и все производные переменные и `file::delete` по-прежнему
считает от **основного** транспорта. Файлы шаблона в этом плане не
меняются — их переводит на список следующий план (этап 3).

**Tech Stack:** Rust (chip-data-gen, edition как в его `Cargo.toml`), Rhai
(хук cargo-generate), тесты хука — `chip-data-gen/tests/cascade.rs`.

**Spec:** `docs/superpowers/specs/2026-10-20-multi-transport-design.md`
(разделы 1, 2; этапы 1–2).

## Global Constraints

- Ветка `feat/multi-transport`; коммиты только с явными путями, без `git add -A`; не пушить.
- Зависимости не добавлять.
- Виды транспорта: `uart`, `can`, `tcp`, `udp`, `usb`; протоколы: `none`, `modbus`, `protobuf`, `canopen` (как в `PROTOCOL_INTERFACES`, `chip-select.rhai:546-551`).
- Вид выбирается столько раз, сколько у чипа доступных блоков его меток (`INTERFACE_BLOCKS`, `chip-select.rhai:535-541`); `tcp` и `udp` — не больше одного раза каждый, оба на одном блоке ETH.
- Протокол в проекте один; транспорт без протокола допустим.
- Блок назначает генерация: следующий свободный из списка чипа, в порядке списка.
- `--define` строгий: опечатка и невозможное — ошибка с объяснением, как сейчас.
- Однотранспортный ответ (`interface`/`protocol` или меню с одним транспортом) даёт те же значения всех прежних переменных, что сейчас: все 33 случая `template-check` проходят.
- Все переменные хука — строки (`"true"`/`"false"` для флагов).
- Проверка репозитория шаблона — список из `MAINTAINING.md:603-612`; `template-check` запускать с `TMP='C:\tcb' TEMP='C:\tcb'` (параллельные сессии делят `%TEMP%`).

## Review Focus

1. Ответ без терминала и без `--define` (CI, `--silent`) — вопрос «Добавить ещё?» берёт `no`, проект без лишних транспортов — тест `no_terminal_adds_no_extra_links` (Task 2).
2. Чип, у которого критерий сигналов сменил первый блок или убрал интерфейс, — однотранспортный проект поменялся бы молча; ловит сверка в Task 1, шаг 6 (останов и доклад, а не коммит).
3. `links` с пробелами или пустыми элементами (`links=uart, tcp:modbus,`) — пробелы вокруг элементов отбрасываются, пустой элемент — ошибка — тест `links_tolerate_spaces_but_not_empty_items` (Task 2).
4. Чип с обеими метками одного вида (`usb`+`otg`, `fdcan`+`bxcan`) — блоки обеих меток идут в счёт вида, порядок меток — как в `INTERFACES[suffix]` — тест `both_tags_of_a_kind_count_as_its_blocks` (Task 2).
5. Транспорт с протоколом не первым (`can`, затем `uart:modbus`) — основной транспорт для прежних скаляров — первый с протоколом — тест `the_primary_link_is_the_first_with_a_protocol` (Task 2).

---

### Task 1: Все блоки чипа в `chip-data/instances.rhai`

**Files:**
- Modify: `chip-data-gen/src/main.rs:815-867` (`interface_block`, `parse_instances`), `:2807-2840` (`format_instances`), тесты `:3323-3397`; тип карты в цикле `:66-128`, `SharedMetadata` `:693-697`
- Modify: `chip-select.rhai:1601-1617` (чтение имени блока)
- Regenerate: `chip-data/instances.rhai`, `chip-data/interfaces.rhai`

**Interfaces:**
- Produces: `INSTANCES[suffix][tag]` — **массив** имён блоков (`["USART1", "USART2", "UART4"]`), в порядке нынешнего ранга (предпочтительный, короче, по имени); первый элемент — нынешний единственный блок.
- Produces: `fn parse_instances(text: &str) -> BTreeMap<&'static str, Vec<String>>`.
- Produces: `interface_block` признаёт блок, только если у него есть выводы нужных сигналов: `usart` — `TX` и `RX`; `fdcan`/`bxcan` — `TX` и `RX`; `usb`/`otg` — `DP` и `DM`; `eth` — хоть один вывод (как сейчас). `parse_interfaces` пользуется тем же критерием.

- [ ] **Step 1: Тесты**

В `instances_name_the_preferred_block_with_pins` (переименовать в `instances_list_blocks_with_pins_in_preference_order`) — вывод с сигналами `TX` и `RX` у usart/can, `DP` и `DM` у otg:

```rust
let found = parse_instances(&text);
assert_eq!(found["usart"], ["USART2", "USART3", "USART10", "LPUART1"]);
assert_eq!(found["otg"], ["USB_OTG_HS"], "FS без выводов не годится");
assert_eq!(found["eth"], ["ETH"]);
assert_eq!(found["fdcan"], ["FDCAN1", "FDCAN2"]);
assert!(!found.contains_key("usb"));
```

Новый `a_block_needs_the_pins_of_its_signals`: `USART1` только с `TX` и `CAN1` (bxcan) только с `RX` — не считаются; `USB_OTG_FS` с `DP`, `DM` — считается; `ETH` с одним выводом — считается. `parse_interfaces` того же текста: `["eth", "otg"]`.

- [ ] **Step 2:** `cargo test --manifest-path chip-data-gen/Cargo.toml --release --bin chip-data-gen` — новые тесты FAIL.

- [ ] **Step 3:** реализовать. `interface_block` проверяет сигналы по строкам `signal: "…"` внутри `pins: &[…]` блока. `parse_instances` собирает `Vec<(rank, name)>` на метку, сортирует по рангу, отдаёт имена. `format_instances` пишет `"usart": ["USART1", "USART2"]`; комментарий шапки — «метка интерфейса: имена блоков в порядке выбора».

- [ ] **Step 4:** тесты из шага 2 — PASS.

- [ ] **Step 5:** хук: в `chip-select.rhai:1608-1611` имя блока — `INSTANCES[suffix][found[0]][0]`, проверка наличия — `INSTANCES[suffix].contains(found[0]) && INSTANCES[suffix][found[0]].len() > 0`.

- [ ] **Step 6: Перегенерировать таблицы и сверить**

Перед прогоном сохранить старые `chip-data/instances.rhai` и `chip-data/interfaces.rhai` в scratchpad. Затем
`cargo run --release --manifest-path chip-data-gen/Cargo.toml`.
Сверить (скрипт в scratchpad, не в репозитории): для каждого суффикса — тот же набор меток в `INTERFACES` и тот же **первый** блок каждой метки в `INSTANCES`, что старое единственное имя.
Expected: расхождений нет. **Есть расхождения — остановиться и доложить список пользователю, не коммитить** (дизайн, раздел 1). Остальные файлы `chip-data/` — в рабочей копии уже есть чужие правки (CRLF), их не коммитить.

- [ ] **Step 7: Проверка**

```
cargo fmt    --manifest-path chip-data-gen/Cargo.toml --check
cargo clippy --manifest-path chip-data-gen/Cargo.toml --all-targets -- -D warnings
cargo test   --manifest-path chip-data-gen/Cargo.toml --release
TMP='C:\tcb' TEMP='C:\tcb' cargo run --release --manifest-path chip-data-gen/Cargo.toml --bin template-check -- --quick
```
Expected: всё зелёное, 33/33.

- [ ] **Step 8: Commit**

```bash
git add chip-data-gen/src/main.rs chip-select.rhai chip-data/instances.rhai chip-data/interfaces.rhai
git commit -m "feat(chip-data): все выведенные блоки интерфейса, а не один"
```

---

### Task 2: Список транспортов в хуке

**Files:**
- Modify: `chip-select.rhai` — таблицы `:532-570`, отсев `:572-608`, вопросы `:944-1028`, переменные `:1055-1071`, `:1601-1626`
- Modify: `chip-data-gen/tests/cascade.rs` — мок `variable::prompt` (`:213-244`), новые тесты рядом с `:1217-1660`
- Modify: `cargo-generate.toml:110-131` (комментарий про `--define`), `MAINTAINING.md:516-520` (порядок вопросов)

**Interfaces:**
- Consumes: `INSTANCES[suffix][tag]` — массив (Task 1).
- Produces (переменные Liquid для этапа 3):
  - `links` — транспорты через запятую в порядке ответов, элемент `вид:метка:блок:протокол:имя` (имя транспорта в проекте — блок строчными, у `tcp`/`udp` — сам вид): `uart:usart:USART1:modbus:usart1,tcp:eth:ETH:modbus:tcp,can:fdcan:FDCAN1:none:fdcan1`; пустая строка — транспорта нет.
  - `has_uart`, `has_can`, `has_tcp`, `has_udp`, `has_usb`, `has_net` (`tcp` или `udp`) — `"true"`/`"false"`.
  - Прежние `interface`, `protocol`, `iface_periph`, `can_kind`, `usb_kind` и все производные (`link_port` … `has_resources`, `chip-select.rhai:1642-1668`) и `file::delete` (`:1675-1808`) — **от основного транспорта**: первого с протоколом, а если таких нет — первого. Их перевод на множество — этап 3, не здесь.
- Produces (ответы):
  - меню `interface:` и `protocol:` — те же id, задаются на каждый транспорт;
  - новый вопрос id `more_links`: `"more_links: добавить ещё один транспорт?"`, варианты `["no — больше транспортов нет", "yes — ещё один транспорт"]`, по умолчанию `no`;
  - `--define links=вид[:протокол],…`.

Правила (все — из дизайна, раздел 2):

- Свободные блоки вида = блоки всех его меток из `INTERFACES[suffix]` (в порядке меток там, внутри метки — порядок `INSTANCES`) минус уже назначенные; `tcp`/`udp` — свободны, если этот вид ещё не выбран и у чипа есть `eth`.
- Меню `interface:` первого транспорта — как сейчас (`none` + виды со свободным блоком). Меню следующих — без `none` (отказ — это `no` на `more_links`). Текст пункта — `INTERFACE_TEXT[вид] + " — " + блок` (блок, который получит транспорт).
- Меню `protocol:` — пока протокол проекта не выбран: как сейчас; когда выбран: `none` и он, если он работает поверх вида; иначе вопрос не задаётся (`none`). `modbus_master` — один раз, когда впервые выбран Modbus.
- `more_links` не задаётся, если ни у одного вида нет свободного блока; после ответа `none` на первом `interface:` — тоже.
- Отсев чипов до каскада: по `links`, если задан, — у чипа свободных блоков каждого вида хватает на запрошенное число; иначе — как сейчас по `interface`.

Ошибки `--define` (текст — начало сообщения, тесты проверяют подстроку):

| Условие | Сообщение |
|---|---|
| `links` вместе с `interface` или `protocol` | `links: задаётся вместо interface и protocol, а не вместе с ними` |
| пустой элемент | `links: пустой элемент в «…»` |
| неизвестный вид | `links: ожидается вид can, tcp, udp, uart, usb, а не «…»` |
| неизвестный протокол | `links: ожидается протокол canopen, modbus, none, protobuf, а не «…»` |
| протокол не работает поверх вида | `links: протокол P работает поверх …, а не «вид»` |
| два разных протокола (кроме `none`) | `links: протокол в проекте пока один, а заданы P1 и P2` |
| вида больше, чем блоков | `links: у <chip> блоков вида K — N (имена), а запрошено M` |

- [ ] **Step 1: Мок с повторными вопросами**

В `run_interview` `picks` для одного id расходуются по порядку: `[("interface", "uart"), ("interface", "tcp")]` отвечает на первый и второй вопрос `interface:`. Вопрос, для которого ответы кончились, ведёт себя как без терминала. Существующие тесты не меняются.

- [ ] **Step 2: Тесты хука** (в `cascade.rs`; чипы и блоки — сверить с `chip-data/instances.rhai` после Task 1)

- `single_answers_keep_the_old_variables` — `c011d6`, `interface=uart`, `protocol=modbus`, `ota=no`: `links == "uart:usart:USART1:modbus:usart1"`, `has_uart == "true"`, `has_can == "false"`, `iface_periph == "USART1"`, `interface == "uart"`.
- `links_assign_blocks_in_order` — `h743zg`, `links=uart:modbus,uart:modbus,tcp:modbus`, `ota=no`: `links == "uart:usart:<1-й>:modbus:<имя 1-го>,uart:usart:<2-й>:modbus:<имя 2-го>,tcp:eth:ETH:modbus:eth"`, `has_net == "true"`, `protocol == "modbus"`.
- `links_reject_impossible_answers` — по строке на каждую ошибку таблицы выше (для «вида больше, чем блоков»: чип с одним CAN, `links=can,can`).
- `links_tolerate_spaces_but_not_empty_items` — `links=uart, tcp:modbus` принимается; `links=uart,,tcp` — ошибка «пустой элемент».
- `the_menu_offers_a_kind_only_while_blocks_remain` — интервью на `f407ve` (у него два bxCAN): `interface=can`, `protocol=none`, `more_links=yes`, `interface=can`, `protocol=none`, `more_links=yes`; в `prompts` второе меню `interface:` содержит пункт `can`, третье — нет.
- `once_a_protocol_is_chosen_only_it_and_none_are_offered` — `uart` + `modbus`, затем `tcp`: второе меню `protocol:` — ровно `none` и `modbus`.
- `a_kind_the_protocol_cannot_carry_gets_no_protocol_question` — `uart`+`modbus`, затем `can`: второго вопроса `protocol:` нет, в `links` — `can:…:none`.
- `the_primary_link_is_the_first_with_a_protocol` — `links=can,uart:modbus` на `f407ve`: `interface == "uart"`, `iface_periph == "USART1"`, `protocol == "modbus"`.
- `both_tags_of_a_kind_count_as_its_blocks` — чип с `usb` и `otg` или с `fdcan` и `bxcan` (найти в `chip-data/interfaces.rhai`; нет таких — тест на чипе с `OTG_FS`+`OTG_HS`, `links=usb,usb`, оба блока в `links`).
- `chips_without_enough_blocks_are_filtered_before_the_cascade` — каскад к чипу с одним CAN при `links=can,can` не доходит (ошибка мока «ни один вариант … не ведёт к цели»), к `f407ve` — доходит.
- `no_terminal_adds_no_extra_links` — интервью без `picks` после первого транспорта: в `links` один элемент, `more_links` был показан один раз.

- [ ] **Step 3:** `cargo test --manifest-path chip-data-gen/Cargo.toml --release --test cascade` — новые тесты FAIL, старые PASS.

- [ ] **Step 4: Реализовать в `chip-select.rhai`**

Функции рядом с `has_block`/`listed` (`:315-330`):
- `fn free_blocks(kind, links, have, instances) -> Array` — массив `#{ tag, block }` свободных блоков вида;
- `fn parse_links(text, chip_name) -> Array` — разбор `--define links`, ошибки из таблицы, кроме «вида больше, чем блоков» (её — при назначении);
- `fn links_text(links) -> String` — значение переменной `links`.

Транспорт — `#{ kind, tag, block, protocol }`. Цикл вопросов заменяет `:944-1004`; `modbus_master` (`:1007-1028`) — без изменений по смыслу, вызывается при первом выборе Modbus. После цикла: `iface`/`protocol` и `iface_periph`/`can_kind`/`usb_kind` (`:1601-1617`) — от основного транспорта; блок основного — из его записи, а не `found[0]`. Выставить `links` и `has_*` рядом с `:1625-1626` и их пустые умолчания рядом с `:1061`.

- [ ] **Step 5:** `cargo test --manifest-path chip-data-gen/Cargo.toml --release` — PASS, включая все прежние тесты каскада.

- [ ] **Step 6: Документация хука**

`cargo-generate.toml:110-131` — ключ `links`, формат, взаимоисключение с `interface`/`protocol`. `MAINTAINING.md:516-520` — цикл вопросов и `more_links`. Отметить в обоих: файлы шаблона пока смотрят на основной транспорт (этап 3).

- [ ] **Step 7: Проверка**

```
cargo fmt    --manifest-path chip-data-gen/Cargo.toml --check
cargo clippy --manifest-path chip-data-gen/Cargo.toml --all-targets -- -D warnings
cargo test   --manifest-path chip-data-gen/Cargo.toml --release
TMP='C:\tcb' TEMP='C:\tcb' cargo run --release --manifest-path chip-data-gen/Cargo.toml --bin template-check
```
Expected: всё зелёное, 33/33 с полной сборкой (однотранспортные проекты не изменились).

- [ ] **Step 8: Commit**

```bash
git add chip-select.rhai chip-data-gen/tests/cascade.rs cargo-generate.toml MAINTAINING.md
git commit -m "feat(hook): несколько транспортов — вопрос more_links и --define links"
```

---

## Дальше (отдельные планы, после этого)

Этап 3 (шаблон по списку) начинается со спайка: даёт ли `liquid` в
`cargo-generate` `split` + `for` по строковой переменной. Этапы 4–8 — по
дизайну; этап 6 начинается с исследования `zencan`.
