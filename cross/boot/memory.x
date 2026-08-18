/* Заполняется вручную: сюда попадает этот файл, только если раскладку не
   удалось посчитать при генерации (или её отключили `--define write_size=...`).
   Обычно chip-select.rhai пишет сюда готовые адреса по секторам чипа. */

MEMORY {
    FLASH             (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    BOOTLOADER_STATE  (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    ACTIVE            (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    DFU               (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    RAM               (xrw) : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    /* Два хвостовых куска RAM, отрезанных от её конца: PERSIST — под данные,
       переживающие сброс (секция .persist ниже), PANIC — под дамп
       panic-persist. Оба обязательны: `#[panic_handler]` в
       cross/app/src/main.rs без символов _panic_dump_* не слинкуется. */
    PERSIST           (xrw) : ORIGIN = /* ADDR END RAM - 2*LEN */, LENGTH = /* LEN */
    PANIC             (xrw) : ORIGIN = /* ADDR END RAM - LEN   */, LENGTH = /* LEN */
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(FLASH);
__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(FLASH);

__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(FLASH);
__bootloader_active_end   = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(FLASH);

__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(FLASH);
__bootloader_dfu_end   = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(FLASH);

/* Данные, переживающие сброс: #[unsafe(link_section = ".persist")]. */
SECTIONS {
    .persist (NOLOAD) : ALIGN(4)
    {
        *(.persist .persist.*);
        . = ALIGN(4);
    } > PERSIST
} INSERT AFTER .uninit

/* panic-persist пишет дамп по голым адресам, не через секцию: */
/* поэтому у него свой регион, а не место внутри .persist. */
_panic_dump_start = ORIGIN(PANIC);
_panic_dump_end   = ORIGIN(PANIC) + LENGTH(PANIC);
