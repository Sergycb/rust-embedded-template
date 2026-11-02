Границы разделов и регионов памяти, которые нужны рычагам платы
({% if shared_flash == "true" %}`bsp::flash`{% endif %}{% if external_flash == "true" %}{% if shared_flash == "true" %}, {% endif %}`bsp::external`{% endif %}{% if dma_nc == "true" %}{% if shared_flash == "true" or external_flash == "true" %}, {% endif %}`bsp::dma_nc`{% endif %}), — одно место чтения на все.

Числа — символы `memory.x`, значение символа — его адрес; проверки над
ними — `ASSERT` там же (docs/flash.md, «Символы раскладки»).{% if ota == "true" %} По тем же символам
прошивка считает и отпечаток раскладки ([`fingerprint`]), который сверяет с
хвостом образа узел OTA.{% endif %}