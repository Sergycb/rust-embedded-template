/* Заполняется вручную: сюда попадает этот файл, только если раскладку не
   удалось посчитать при генерации (или её отключили `--define write_size=...`).
   Обычно chip-select.rhai пишет сюда готовые адреса по секторам чипа. */

MEMORY {
    FLASH             (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    BOOTLOADER_STATE  (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    ACTIVE            (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    DFU               (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    RAM               (xrw) : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    /* Хвост RAM, отрезанный от её конца: PANIC — дамп паники приложения.
       Bootloader в него не пишет, но и стеком своим не трогает: дамп
       доживает до следующего старта `app`. */
    PANIC             (xrw) : ORIGIN = /* ADDR END RAM - LEN */, LENGTH = /* LEN */
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(FLASH);
__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(FLASH);

__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(FLASH);
__bootloader_active_end   = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(FLASH);

__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(FLASH);
__bootloader_dfu_end   = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(FLASH);
{%- if ota_store == "external" %}
{%- if config_store == "external" %}

/* Раздел настроек на внешней флеши — те же смещения, что в memory.x приложения. */
__config_start = /* 0xXXXXXXXX */;
__config_end   = /* 0xXXXXXXXX */;
{%- endif %}

/* Конец последнего раздела на внешней флеши (bsp::layout): микросхема меньше
   не годится. */
__ext_flash_end = {% if config_store == "external" %}MAX(MAX(__bootloader_state_end, __bootloader_dfu_end), __config_end){% else %}MAX(__bootloader_state_end, __bootloader_dfu_end){% endif %};
{%- endif %}
