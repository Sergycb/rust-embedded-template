Флеш платы: общий замок над контроллером и куски флеша, которые плата отдаёт
узлам.

Контроллер флеша на чипе один, а работать с ним нужно всем сразу{% if ota == "true" and ota_store != "external" %}: флешу под
обновлением (`DFU` и `BOOTLOADER_STATE`){% endif %}{% if config == "true" and config_store == "flash" %}, разделу `CONFIG` настроек{% elsif config == "true" and config_store == "eeprom" %}, EEPROM настроек (она пишется через тот же контроллер){% endif %}. Поэтому объект
создаётся ровно один раз (`flash::new`, ему отдаётся группа `flash` из
`resources.rs`), лежит в `static` под блокирующим замком (`FlashMutex`), а
куски флеша получают на него ссылки.

Куски — рычаги платы, полями `Board`:
{%- if ota == "true" and ota_store != "external" %}

- `firmware` (`FirmwareFlash` — `adapters::ota::Linked`) — флеш под
  обновлением, ещё не нарезанный: носители `DFU` и `BOOTLOADER_STATE` со своими
  страницами и размер `ACTIVE` (`ACTIVE_LEN` из `[env]`, разбирает `bsp::layout`). Где в
  нём лежат разделы, знает алгоритм обновления, а не плата: их находит узел OTA
  по символам `memory.x` (`FirmwareUpdaterConfig::from_linkerfile`), когда
  собирает адаптер (`adapters::ota::Updater`, порт `ports::FirmwareLayout`).
  Обмен копирует ровно `ACTIVE`, поэтому предел образа — он, а не `DFU`.
{%- if state_paged == "true" %} Носитель
  `BOOTLOADER_STATE` — `flash_page::ErasePage` со страницей своего сектора
  (`STATE_ERASE_SIZE`): над цельным `Flash` раздел не кратен его странице.{% else %} Сектор `BOOTLOADER_STATE` равен странице
  цельного `Flash`, и носитель — он сам (равенство проверяет `const`-assert).{% endif %}
{%- if swap_paged == "true" %} Носитель `DFU` — `ErasePage` со страницей
  обмена `SWAP_PAGE_SIZE`, крупнее физической: так журнал `BOOTLOADER_STATE`
  короче, и коду bootloader'а остаётся почти весь его блок.{% endif %}{% if dfu_bank == "true" %} `DFU` — в банке 2 за разрывом
  адресов (`DfuBankMutex`): цельный `Flash` туда не дотягивается.{% endif %} Те же
  числа (`STATE_ERASE_SIZE`{% if swap_paged == "true" %}, `SWAP_PAGE_SIZE`{% endif %}) берёт отсюда bootloader — разделам в
  приложении и в нём расходиться негде.
{%- endif %}
{%- if config == "true" and config_store == "flash" %}

- `config` (`ConfigFlash`) — раздел `CONFIG` (границы — `CONFIG_START`/`CONFIG_END`
  из `[env]`, страница — `CONFIG_ERASE_SIZE`{% if config_paged == "true" %}, над `ErasePage`{% endif %}), над которым
  узел-потребитель собирает хранилище (`adapters::settings`). Именно раздел:
  запись за его границы отказывает, а не стирает {% if ota == "true" %}`ACTIVE` или bootloader{% else %}код приложения{% endif %}.
{%- elsif config == "true" and config_store == "eeprom" %}

- `config` (`ConfigFlash` — `adapters::eeprom::Eeprom` над `EepromFlash`) —
  внутренняя EEPROM как `NorFlash`, под замком того же контроллера; над ней
  узел-потребитель собирает хранилище (`adapters::settings`).
{%- endif %}

Носители строятся над разделом на весь флеш (`Whole`,
`BlockingPartition::new(flash, 0, SIZE)`): смещения те же, что у самого
{%- assign ota_in = "" %}{% if ota == "true" and ota_store != "external" %}{% assign ota_in = "1" %}{% endif %}
{%- assign cfg_in = "" %}{% if config == "true" and config_store == "flash" %}{% assign cfg_in = "1" %}{% endif %}
флеша, операции идут под замком, по одной{% if ota_in == "1" or cfg_in == "1" %}; асинхронный вид для {% if ota_in == "1" %}`embassy-boot`{% if cfg_in == "1" %} и
{% endif %}{% endif %}{% if cfg_in == "1" %}`sequential-storage`{% endif %} даёт `BlockingAsync`{% endif %}. `BlockingPartition::new` паникует,
если объём не кратен `ERASE_SIZE`; у STM32 он всегда кратен самому крупному
сектору, и `const`-assert здесь{% if ota == "true" %} и в `boot`{% endif %} проверяет это при сборке.

`NoopRawMutex` подразумевает один исполнитель — подробности в `docs/flash.md`.
