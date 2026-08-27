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
       переживающие сброс (адресуются символами ниже), PANIC — под дамп
       panic-persist. Оба обязательны: `#[panic_handler]` в
       crates-cross/app/src/main.rs без символов _panic_dump_* не слинкуется. */
    PERSIST           (xrw) : ORIGIN = /* ADDR END RAM - 2*LEN */, LENGTH = /* LEN */
    PANIC             (xrw) : ORIGIN = /* ADDR END RAM - LEN   */, LENGTH = /* LEN */
}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - ORIGIN(FLASH);
__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - ORIGIN(FLASH);

__bootloader_active_start = ORIGIN(ACTIVE) - ORIGIN(FLASH);
__bootloader_active_end   = ORIGIN(ACTIVE) + LENGTH(ACTIVE) - ORIGIN(FLASH);

__bootloader_dfu_start = ORIGIN(DFU) - ORIGIN(FLASH);
__bootloader_dfu_end   = ORIGIN(DFU) + LENGTH(DFU) - ORIGIN(FLASH);

/* Аппаратное начало RAM — впишите тот же адрес, что и в ORIGIN(RAM) выше.
   Именно литералом, а не `ORIGIN(RAM)`: flip-link переопределяет блок MEMORY,
   сдвигая начало вверх на размер статики, и после него `ORIGIN(RAM)` означает
   вершину стека, а не дно — замер стека (bsp::stack) тогда всегда даёт ноль. */
_ram_start = /* 0xXXXXXXXX */;

/* Данные, переживающие сброс: адресуются через эти символы. Своей секции у
   них нет намеренно — секция с VMA в конце RAM убеждает flip-link, что
   свободного места не осталось, и он оставляет стек в самом начале RAM;
   прошивка после этого уходит в HardFault на первом же push. */
_persist_start = ORIGIN(PERSIST);
_persist_end   = ORIGIN(PERSIST) + LENGTH(PERSIST);

/* Сюда пишет panic-persist — тоже по голым адресам, без секции. */
_panic_dump_start = ORIGIN(PANIC);
_panic_dump_end   = ORIGIN(PANIC) + LENGTH(PANIC);
