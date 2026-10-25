---
paths: crates-cross/**
---

- Задачи embassy не могут быть generic: объект, уезжающий в
  `#[embassy_executor::task]`, обязан иметь псевдоним типа (рычагу платы — в
  `bsp`{% if ota == "true" or config == "true" %}: {% if ota == "true" %}`bsp::FirmwareFlash`{% if config == "true" %}, {% endif %}{% endif %}{% if config == "true" %}`bsp::ConfigFlash`{% endif %}{% endif %}; адаптеру — в модуле `app`,
  который выбирает его тип{% if protocol_link == "true" %}, `crate::link`{% endif %}, или конкретный тип прямо во входе узла{% if ota == "true" or config == "true" %}:
  {% if ota == "true" %}`adapters::ota::Updater<bsp::FirmwareFlash, _>`{% if config == "true" %}, {% endif %}{% endif %}{% if config == "true" %}`adapters::settings::Settings<bsp::ConfigFlash>`{% endif %}{% endif %}).
- Ровно один `#[panic_handler]` и один `#[global_logger]` на бинарник — это
  ограничение линкера, а не Cargo. Развилка по профилю делается через
  `#[cfg(debug_assertions)]` (паникёр — `app/src/panic.rs`{% if ota == "true" %}; у `boot` свой, сброс
  без сообщения{% endif %}), не Cargo-фичей.
- `Board` (один, `Board::new() -> Board`) отдаёт рычаги платы — объекты под
  трейтами (`embedded-io-async`, `embedded-storage`, порты `ports`): потоки, шины,
  разделы флеша, драйверы микросхем платы — и данные о чипе (`bsp::ChipSpec`), а не
  `Peripherals`, не пины, не периферию ядра и не `Clocks`.{% if has_usb == "true" or has_net == "true" %} Пары из одного
  конструктора собирает плата: {% if has_usb == "true" %}`board.<usb>: Usb { device, cdc }`{% if has_net == "true" %}, {% endif %}{% endif %}{% if has_net == "true" %}`board.net: Net
  { runner, stack }`{% endif %}.{% endif %} Протоколы над рычагами собирает узел `domain` через `ports::Assemble`; `app` называет тип
  адаптера, заводит статики с его буферами и раздаёт поля платы, а объектов не
  строит.{% if graph == "true" %} Исключение (сторож) и его причина — в `docs/architecture.md`; новых не
  заводить.{% endif %}
- `bsp` ничего не инициализирует сверх подъёма HAL: конструкторы зовутся `new`
  и только создают объекты. Всё, что запускает железо{% if graph == "true" %} (`arm()` у сторожа){% endif %},
  делает тот, кто этим железом распоряжается. Исключения — `ensure_*` для
  option bytes, `bsp::cache::enable()` (кэши чипа) и `Cycles::new` (счётчик
  тактов) —
  `docs/clocks.md`{% if external_flash == "true" %}; и `init` внешней флеши в `bsp::external::new` (опознание
  микросхемы — без него драйвер не годится ни для одной операции), из-за него
  `Board::new` асинхронный и зовётся из `main` через `block_on` —
  `docs/modules/bsp-external.md`{% endif %}.
- `lib.rs` в `bsp` — только объявления модулей и реэкспорт; код живёт в
  модулях. Модуль без кода (одна `#![doc = include_str!(..)]`) не заводить:
  такой документации место в `docs/`.
- `app` про `embassy-stm32` не знает — ни `use`, ни строки в манифесте. Ядро
  `app` берёт напрямую (`cortex-m-rt` — точка входа и `HardFault`, `cortex-m` —
  свободные функции ядра), без обёрток в `bsp`; паника и фолт — в `app`. Сброс МК —
  рычаг `bsp::reset::Reboot` (на M7 он чистит кэш данных перед сбросом).
{%- capture wrappers %}{% if has_usb == "true" %}, обёртки USB CDC{% endif %}{% if has_net == "true" %}, `Net`{% endif %}{% if config_store == "eeprom" %}, EEPROM{% endif %}{% endcapture %}
{%- capture levers %}{% if wrappers != "" %}, `adapters` ({{ wrappers | remove_first: ", " }}){% endif %}{% if has_usb == "true" %}, `embassy-usb`{% endif %}{% if has_net == "true" %}, `embassy-net`{% endif %}{% if net_services contains "ssh" %}, `getrandom`{% endif %}{% endcapture %}
- `bsp` зависит от `ports`{% if flash_page == "true" %} (и `flash-page`){% endif %}{% if levers != "" %} и от того, из чего собирает свои рычаги:
  {{ levers | remove_first: ", " }}, —{% else %},{% endif %} но не от `domain`: входы узлов из полей `Board` заполняет `app`.{% if ota == "true" %} `boot` зовёт `domain` только при `signed`{% if ota_store == "external" %} или
  `ext_flash=yes`{% endif %} и только `domain::ota::boot::{boot_plan, screen_update}`.{% endif %}
- Логирование только `defmt`; `log` в прошивке не используется.
- **Если вы в репозитории самого шаблона** (признак — каталог `chip-data-gen/`
  в корне): файлы здесь содержат Liquid и до генерации не являются валидным
  Rust, правки проверяются генерацией (`template-check`), а не `cargo build`.
  Уровень — по правке (`MAINTAINING.md`, «Какой уровень когда»): `--quick`
  на каждую, `--only <ветка>` или `--smoke` по ходу работы, полный прогон —
  перед вливанием в master.
  В остальных случаях (сгенерированный проект) это обычный Rust — собирайте и
  проверяйте как любой другой код.
