Флеш платы: общий замок над контроллером.

Контроллер флеша на чипе один, а работать с ним нужно всем сразу{% if ota == "true" and ota_store != "external" %}: разделам
`DFU` и `BOOTLOADER_STATE` обновления{% endif %}{% if config == "true" and config_store == "flash" %}, разделу `CONFIG` настроек{% elsif config == "true" and config_store == "eeprom" %}, EEPROM настроек (она пишется через тот же контроллер){% endif %}. Поэтому объект
создаётся ровно один раз (`flash::new`, ему отдаётся блок `FLASH` из группы
`flash` в `resources.rs`), лежит в `static` под блокирующим замком
(`FlashMutex`), а потребители получают на него ссылки.

Каждый потребитель строит свой раздел на месте:
`BlockingPartition::new(flash, 0, flash::SIZE)` из `embassy-embedded-hal` — раздел
на весь флеш с нулевым смещением ([`Whole`]). Операции идут под замком, по
одной.{% if erase_page == "true" %} Если нужна другая страница стирания (`STATE`, `CONFIG`, страница
обмена), поверх раздела ставится `flash_page::ErasePage`.{% endif %} Асинхронный
вид для `embassy-boot` и `sequential-storage` даёт
`embassy_embedded_hal::adapter::BlockingAsync`. `BlockingPartition::new`
паникует, если объём не кратен `ERASE_SIZE`; у STM32 он всегда кратен самому
крупному сектору, и `const`-assert в `bsp::flash` и `boot` проверяет это при
сборке{% if dfu_bank == "true" %} (банк 2 на этом чипе — отдельный раздел, `flash::DFU_BANK_SIZE`){% endif %}.

`NoopRawMutex` подразумевает один исполнитель — подробности в `docs/flash.md`.
