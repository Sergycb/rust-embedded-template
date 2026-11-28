Границы разделов, которые нужны приложению, — одно место чтения на всё:
`app::layout`{% if ota == "true" %} (и его локальная копия `boot::layout` в `crates-cross/boot`){% endif %}.

Числа — символы `memory.x` этого бинарника, значение символа — его адрес;
проверки над ними — `ASSERT` там же (docs/flash.md, «Символы раскладки»).{% if ota == "true" %} По тем
же символам `app::layout::fingerprint` считает отпечаток раскладки, который
сверяет с хвостом образа узел OTA.{% endif %}

`app::layout` отдаёт границы разделов `app::storage`:

- `active_len` — размер `ACTIVE` (предел образа){% if ota == "true" %}, `fingerprint` — отпечаток
  раскладки{% endif %};
{%- if ota == "true" %}
- `dfu`/`state` — разделы обновления; с `DFU` в банке 2 `dfu` отсчитан от его
  начала;
{%- endif %}
{%- if config == "true" %}
- `config` — раздел `CONFIG` во флеше или на внешней флеши;
{%- endif %}
{%- if external_flash == "true" %}
- `ext_flash_end` — конец последнего раздела на внешней флеши: меньше
  микросхема не годится (`cpu::external::init` сверяет с ним объём).
{%- endif %}
{% if ota == "true" %}
`boot::layout` — та же нарезка для bootloader'а по его `memory.x` плюс
`fingerprint`.
{%- endif %} Регионы `DMA_NC`/`SRAM4` здесь не читаются: `cpu::dma_nc` берёт
их границы числами из генерации.
