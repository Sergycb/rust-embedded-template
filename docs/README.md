# Документация проекта

«Почему так решено» — здесь. «Что делать» — в `AGENTS.md` (команды, запреты,
чек-лист). Как устроен проект для человека — в `README.md` в корне.

| Файл | О чём | Когда открывать |
|---|---|---|
| `architecture.md` | границы `domain`/`cross`, что пересекает границу `bsp` → `app`, `chip-info` | добавляете код и не знаете, в какой крейт |
| `first-node.md` | свой узел по шагам: фрагмент и автомат в `domain`, фича слова DSL `supervisor`, {% if graph == "true" %}запись в графе, сторож{% else %}модуль задачи в `app`{% endif %}, тест на хосте | заводите новую подсистему со своей задачей |
| `clocks.md` | дамп частот, `EXPECTED_SYS_HZ`, кэши и `DMA_NC`, ядро (фолты, ловушка деления), `CPU_FREQ_BOOST`, пороги вроде `IMAGE_BUDGET_PERCENT` | трогаете тактирование, кэши, обработчик фолтов или порог проверки |
{%- if ota == "true" %}
| `ota.md` | приём образа, подпись, защита от отката, тест полного цикла | трогаете `domain::ota::Updater`, {% if shared_flash == "true" %}`bsp::flash`, {% endif %}`domain::ota`, `domain::ota::download` или `domain::ota::update` |
{%- endif %}
| `flash.md` | {% if ota == "true" %}стёртое состояние (`0xFF` против `0x00`), {% endif %}{% if config == "true" %}раздел настроек, {% endif %}{% if shared_flash == "true" %}один `Flash` на всех, {% endif %}банки flash{% if ota_store == "external" %}, OTA с разделами на внешней флеши{% endif %} | трогаете {% if config == "true" %}`domain::settings`, {% endif %}{% if erase_page == "true" %}`flash_page`, {% endif %}{% if shared_flash == "true" %}`bsp::flash`, {% endif %}{% if ota == "true" %}`domain::ota`{% if graph == "true" %} (его `local`-слоты связаны с типом мьютекса){% endif %}, {% endif %}раскладку памяти |
{%- if graph == "true" %}
| `watchdog.md` | связанные цепочкой таймауты узлов, почему узел `observe`, почему взводится не в `init` | трогаете сторож или тайминги графа задач |
{%- endif %}
| `diagnostics.md` | паникёр по профилю, регион `PANIC`, причина сброса, чего в диагностике нет ({% if ota == "true" %}причины падений bootloader'а, {% endif %}замера стека) | трогаете паникёр, флаги сброса, `cargo xtask panic-info` или раскладку RAM |
| `conventions.md` | feature-флаги, правила добавления зависимостей, host-тесты только через nextest, правило `lib.rs`, порядок объектов в модуле, осознанные ограничения (тулчейн, воспроизводимый релиз) | добавляете зависимость или фичу, поднимаете тулчейн, тест с часами нестабилен |
| `resources.md` | `assign_resources!`, куда его класть, что в группы не включать | заполняете распиновку своей платы |
| `buffers.md` | рецепт zero-copy DMA-буфера на `bbqueue` (зависимости в шаблоне нет), чем он не `embassy_sync::Pipe` | заводите статический буфер под периферию |
{%- if net_services contains "mqtt" %}

`modules/app-mqtt.md` — служба MQTT: темы, `mqtt.toml`, как добавить поле и команду, ACL брокера.
{%- endif %}

`modules/` — документация модулей, подключённая в код через
`#![doc = include_str!(...)]`{% if net_services contains "mqtt" %} (кроме `app-mqtt.md` — он отдельный, см. выше){% endif %}. Правится вместе с кодом модуля, а не отдельно.
