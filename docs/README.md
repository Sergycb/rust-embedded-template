# Документация проекта

«Почему так решено» — здесь. «Что делать» — в `AGENTS.md` (команды, запреты,
чек-лист). Как устроен проект для человека — в `README.md` в корне.

| Файл | О чём | Когда открывать |
|---|---|---|
| `architecture.md` | границы `domain`/`cross`, что пересекает границу `bsp` → `app`, `chip-info` | добавляете код и не знаете, в какой крейт |
| `first-node.md` | свой узел по шагам: фрагмент и задача в `domain`, фича слова DSL `supervisor`, {% if graph == "true" %}запись в графе, сторож{% else %}модуль задачи в `app`{% endif %}, тест на хосте | заводите новую подсистему со своей задачей |
| `clocks.md` | дамп частот, `EXPECTED_SYS_HZ`, кэши и `DMA_NC`, ядро (фолты, ловушка деления), `CPU_FREQ_BOOST`, пороги вроде `IMAGE_BUDGET_PERCENT` | трогаете тактирование, кэши, обработчик фолтов или порог проверки |
{%- if ota == "true" %}
| `ota.md` | приём образа, подпись, защита от отката, тест полного цикла | трогаете `domain::ota::Updater`, {% if shared_flash == "true" %}`bsp::flash`, {% endif %}`domain::ota`, `domain::ota::download` или `domain::ota::update` |
{%- endif %}
| `flash.md` | {% if ota == "true" %}стёртое состояние (`0xFF` против `0x00`), {% endif %}{% if config == "true" %}раздел настроек, {% endif %}{% if shared_flash == "true" %}один `Flash` на всех, {% endif %}банки flash{% if ota_store == "external" %}, OTA с разделами на внешней флеши{% endif %} | трогаете {% if config == "true" %}`domain::settings`, {% endif %}{% if erase_page == "true" %}`flash_page`, {% endif %}{% if shared_flash == "true" %}`bsp::flash`, {% endif %}{% if ota == "true" %}`domain::ota`{% if graph == "true" %} (его `local`-слоты связаны с типом мьютекса){% endif %}, {% endif %}раскладку памяти |
{%- if graph == "true" %}
| `watchdog.md` | связанные цепочкой таймауты узлов, почему узел `observe`, почему взводится в `spawn_all`, а не в `Board::new` | трогаете сторож или тайминги графа задач |
{%- endif %}
| `diagnostics.md` | паникёр по профилю, регион `PANIC`, причина сброса, чего в диагностике нет ({% if ota == "true" %}причины падений bootloader'а, {% endif %}замера стека) | трогаете паникёр, флаги сброса, `cargo xtask panic-info` или раскладку RAM |
| `conventions.md` | feature-флаги, правила добавления зависимостей, host-тесты только через nextest, правило `lib.rs`, фейки (`mod fake`), порядок объектов в модуле, осознанные ограничения (тулчейн, воспроизводимый релиз) | добавляете зависимость или фичу, поднимаете тулчейн, тест с часами нестабилен, пишете фейк для теста |
| `resources.md` | `assign_resources!`: какие группы уже есть, как добавить свою и собрать из неё рычаг `Board` | заполняете распиновку своей платы |
| `buffers.md` | рецепт zero-copy DMA-буфера на `bbqueue` (зависимости в шаблоне нет), чем он не `embassy_sync::Pipe` | заводите статический буфер под периферию |

`modules/` — документация модулей, подключённая в код через `#![doc = include_str!(...)]`. Правится вместе с кодом модуля, а не отдельно.{% if ota == "true" or net_services contains "mqtt" %} Исключения: {% if net_services contains "mqtt" %}`app-mqtt.md` — отдельный, без `include_str!`{% endif %}{% if ota == "true" %}{% if net_services contains "mqtt" %}; {% endif %}`boot/` описывается в `README.md` и `docs/ota.md`{% endif %}.{% endif %}

| Модуль | О чём | Когда открывать |
|---|---|---|
| `app.md` | app-оркестрация, `main`, `spawn_all` | правите app-entry/оркестрацию |
| `app-cpu-load.md` | замер загрузки, цикл исполнителя | правите замер/тайминги |
| `app-graph.md` | граф узлов, слоты, `static`, watchdog | правите граф/сборку узлов |
{%- if net_services contains "mqtt" %}
| `app-mqtt.md` | MQTT: темы, `mqtt.toml`, ACL, добавление полей/команд | правите MQTT (отдельный, без `include_str!`) |
{%- endif %}
| `bsp-can.md` | CAN-рычаг, заглушка до драйвера платы | правите CAN |
{%- if config_store == "eeprom" %}
| `bsp-eeprom.md` | внутренняя EEPROM как носитель настроек | правите настройки на EEPROM |
{%- endif %}
| `bsp-flash.md` | флеш, разделы, страницы, банки | правите флеш/раскладку |
| `bsp-net.md` | сеть, стек, сокеты, счётчик сокетов | правите сеть |
| `bsp-external.md` | внешняя флеш W25Q, init, заглушка шины | правите внешнюю флеш |
| `bsp-rng.md` | RNG, затравка, getrandom | правите RNG/сеть/SSH |
| `bsp-serial.md` | UART/транспорты, заглушка | правите UART |
| `bsp-usb.md` | USB/CDC, пары | правите USB |
| `bsp-wdg.md` | сторож, арма, таймауты | правите watchdog |
| `board.md` | Board, ChipSpec, сборка, граница bsp->app | правите board/рычаги платы |
| `reset.md` | Reboot, причина сброса RCC | правите reset/паникёр |
| `layout.md` | символы memory.x, разделы, fingerprint | правите раскладку/OTA |
| `fault.md` | HardFault, ловушка деления | правите обработку фолтов |
| `panic.md` | паникёр, panic-persist, дефолтный транспорт | правите паникёр/диагностику |
