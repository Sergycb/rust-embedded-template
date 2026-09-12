/* Заполняется вручную: сюда попадает этот файл, только если раскладку не
   удалось посчитать при генерации (или её отключили `--define write_size=...`).
   Обычно chip-select.rhai пишет сюда готовые адреса по секторам чипа. */

MEMORY {
    FLASH             (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    BOOTLOADER_STATE  (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    ACTIVE            (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    DFU               (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    RAM               (xrw) : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    /* Хвост RAM, отрезанный от её конца: PANIC — под дамп panic-persist
       (паникёр release-профиля). Обязателен: без символов _panic_dump_*
       release-образ не слинкуется. */
    PANIC             (xrw) : ORIGIN = /* ADDR END RAM - LEN */, LENGTH = /* LEN */
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(FLASH);
__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(FLASH);

__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(FLASH);
__bootloader_active_end   = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(FLASH);

__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(FLASH);
__bootloader_dfu_end   = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(FLASH);

/* Сюда пишет panic-persist — по голым адресам, без секции: секция здесь
   ломает flip-link, см. docs/diagnostics.md. */
_panic_dump_start = ORIGIN(PANIC);
_panic_dump_end   = ORIGIN(PANIC) + LENGTH(PANIC);
