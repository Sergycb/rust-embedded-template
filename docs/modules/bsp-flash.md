Флеш платы: общий замок над контроллером и куски флеша, которые плата отдаёт
узлам.

{% capture flash_users %}{% if ota == "true" and ota_store != "external" %}, флешу под
обновлением (`DFU` и `BOOTLOADER_STATE`){% endif %}{% if config == "true" and config_store == "flash" %}, разделу `CONFIG` настроек{% elsif config == "true" and config_store == "eeprom" %}, EEPROM настроек (она пишется через тот же контроллер, модуль — `bsp::eeprom`){% endif %}{% endcapture %}Контроллер флеша на чипе один, и работают с ним: {{ flash_users | remove_first: ", " }}. Поэтому объект
создаётся ровно один раз (`flash::new`, ему отдаётся группа `flash` из
`resources.rs`), лежит в `static` под блокирующим замком (`Lock`), а
куски флеша получают на него ссылки.

Куски — рычаги платы, полями `Board`:
{%- if ota == "true" and ota_store != "external" %}

- `firmware` (`FirmwareFlash` — `ports::Partitions`) — разделы
  обновления: `DFU` и `BOOTLOADER_STATE` со своими страницами, нарезанные по
  символам `memory.x` (`bsp::layout::{dfu, state}` — те же, что читает
  `from_linkerfile` в bootloader'е), размер `ACTIVE` и отпечаток раскладки.
  Над ними compose-site узла OTA строит адаптер (`domain::ota::Updater`).
  Обмен копирует ровно `ACTIVE`, поэтому предел образа — он, а не `DFU`.
{%- if state_paged == "true" %} Носитель
  `BOOTLOADER_STATE` — `flash_page::ErasePage` со страницей своего сектора
  (`STATE_ERASE_SIZE`).{% else %} Сектор `BOOTLOADER_STATE` равен странице
  цельного `Flash`, и носитель — он сам (равенство проверяет `const`-assert).{% endif %}
{%- if swap_paged == "true" %} Носитель `DFU` — `ErasePage` со страницей
  обмена `SWAP_PAGE_SIZE`, крупнее физической.{% endif %}{% if dfu_bank == "true" %} `DFU` — в банке 2 за разрывом
  адресов (`dfu::BankMutex`).{% endif %}{% if state_paged == "true" or swap_paged == "true" %} Те же
  числа ({% if state_paged == "true" %}`STATE_ERASE_SIZE`{% if swap_paged == "true" %}, {% endif %}{% endif %}{% if swap_paged == "true" %}`SWAP_PAGE_SIZE`{% endif %}) берёт отсюда bootloader{% else %} Bootloader строит
  `STATE` над тем же цельным `Flash`, и его страницу держит `const`-assert
  отсюда (bootloader линкует `bsp`){% endif %}; почему страницы такие —
  `docs/flash.md`.
{%- endif %}
{%- if config == "true" and config_store == "flash" %}

- `config` (`ConfigFlash`) — раздел `CONFIG` (границы — символы `memory.x`
  `__config_start`/`__config_end`, читает `bsp::layout`; страница —
  `CONFIG_ERASE_SIZE`{% if config_paged == "true" %}, над `ErasePage`{% endif %}, кратность ей — `ASSERT` в `memory.x`), над которым
  compose-site узла-потребителя строит хранилище (`domain::settings`). Именно раздел:
  запись за его границы отказывает, а не стирает {% if ota == "true" %}`ACTIVE` или bootloader{% else %}код приложения{% endif %}.
{%- elsif config == "true" and config_store == "eeprom" %}

- `config` (`ConfigFlash`) — внутренняя EEPROM как `NorFlash`; носитель,
  обёртка над замком и `EepromFlash` — в `bsp::eeprom` (`docs/modules/bsp-eeprom.md`).
{%- endif %}
{%- assign ota_in = "" %}{% if ota == "true" and ota_store != "external" %}{% assign ota_in = "1" %}{% endif %}
{%- assign cfg_in = "" %}{% if config == "true" and config_store == "flash" %}{% assign cfg_in = "1" %}{% endif %}
{%- if ota_in == "1" or cfg_in == "1" %}

Носители строятся над разделом на весь флеш (`Whole`,
`BlockingPartition::new(flash, 0, SIZE)`): смещения те же, что у самого
флеша, операции идут под замком, по одной; асинхронный вид для {% if ota_in == "1" %}`embassy-boot`{% if cfg_in == "1" %} и
{% endif %}{% endif %}{% if cfg_in == "1" %}`sequential-storage`{% endif %} даёт `BlockingAsync`. `BlockingPartition::new` паникует,
если объём не кратен `ERASE_SIZE`; у STM32 он всегда кратен самому крупному
сектору, и {% if dfu_bank == "true" %}здесь раздел — банк 1 (банк 2 с `DFU` — отдельный раздел), так что
`const`-assert'а на объём нет ни здесь, ни в `boot`{% else %}`const`-assert здесь{% if boot_partitioned == "true" %} и в `boot`{% endif %} проверяет это при сборке{% endif %}.
{%- else %}

Внутренняя EEPROM настроек ходит к тому же контроллеру через [`Lock`] — её
носитель собран в `bsp::eeprom` (`docs/modules/bsp-eeprom.md`).
{%- endif %}

`NoopRawMutex` подразумевает один исполнитель — подробности в `docs/flash.md`.
