/* Заполняется вручную: этот файл остаётся в проекте, только когда раскладку
   отключили при генерации (`--define write_size=...`) или выбранное в неё не
   поместилось. Обычно chip-select.rhai пишет сюда готовые адреса по секторам
   чипа. Заполнив crates-cross/app/memory.x, скопируйте его целиком в
   crates-cross/target-tests/memory.x: тесты на устройстве прошиваются на место
   приложения, и `cargo xtask lint cross` сверяет эти два файла побайтно.

   Форма повторяет сгенерированную, и отступать от неё не надо — ниже
   объяснено, где это стоит рабочей прошивки. */

MEMORY {
{%- if ota == "true" %}
    /* FLASH здесь — это раздел ACTIVE, а НЕ весь чип: приложение линкуется в
       него, а с базы flash стартует bootloader (crates-cross/boot). Впишете
       сюда базу — прошивка приложения затрёт bootloader. */
    FLASH             (rx)  : ORIGIN = /* 0xXXXXXXXX — начало ACTIVE */, LENGTH = /* XXXK */
{%- if ota_store != "external" %}
    BOOTLOADER_STATE  (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    DFU               (rx)  : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
{%- endif %}
{%- else %}
    /* Без OTA приложение — единственный образ: FLASH — весь flash чипа от его
       базы (обычно 0x08000000). */
    FLASH             (rx)  : ORIGIN = /* 0xXXXXXXXX — база flash */, LENGTH = /* XXXK */
{%- endif %}
    RAM               (xrw) : ORIGIN = /* 0xXXXXXXXX */, LENGTH = /* XXXK */
    /* Хвост RAM, отрезанный от её конца: PANIC — под дамп panic-persist
       (паникёр release-профиля). Обязателен: без символов _panic_dump_*
       release-образ не слинкуется. */
    PANIC             (xrw) : ORIGIN = /* ADDR END RAM - LEN */, LENGTH = /* LEN */
}
{%- if ota == "true" %}

/* База flash всего чипа, обычно 0x08000000 — впишите литералом.

   Именно она, а не `ORIGIN(FLASH)`: смещения `__bootloader_*` читает
   embassy-boot, работающий с флешем целиком, а `ORIGIN(FLASH)` выше — это
   ACTIVE. Подставив его, вы получили бы отрицательное смещение, свёрнутое в
   что-нибудь вроде 0xFFFFF800, — и bootloader искал бы своё состояние за
   границей чипа. В crates-cross/boot/memory.x те же строки написаны через
   `ORIGIN(FLASH)` и там верны: у бутлоадера FLASH и правда начинается с базы. */
__flash_base = /* 0xXXXXXXXX */;

/* ACTIVE — это и есть FLASH данного крейта. */
__bootloader_active_start = ORIGIN(FLASH) - __flash_base;
__bootloader_active_end   = ORIGIN(FLASH) + LENGTH(FLASH) - __flash_base;
{%- if ota_store == "external" %}

/* STATE и DFU — на внешней флеши (драйвер в bsp и boot): смещения от её
   начала, а не адреса — регионов в MEMORY у них нет, в адресном пространстве
   МК внешней флеши нет. Границы кратны странице обмена EXT_PAGE_SIZE
   ([env] crates-cross/.cargo/config.toml), DFU длиннее ACTIVE ровно на неё;
   те же числа — в crates-cross/boot/memory.x. */
__bootloader_state_start = /* 0xXXXXXXXX */;
__bootloader_state_end   = /* 0xXXXXXXXX */;

__bootloader_dfu_start = /* 0xXXXXXXXX */;
__bootloader_dfu_end   = /* 0xXXXXXXXX */;
{%- else %}

__bootloader_state_start = ORIGIN(BOOTLOADER_STATE) - __flash_base;
__bootloader_state_end   = ORIGIN(BOOTLOADER_STATE) + LENGTH(BOOTLOADER_STATE) - __flash_base;

__bootloader_dfu_start = ORIGIN(DFU) - __flash_base;
__bootloader_dfu_end   = ORIGIN(DFU) + LENGTH(DFU) - __flash_base;
{%- endif %}
{%- endif %}
{%- if config_store == "external" %}

/* Раздел настроек на внешней флеши (bsp::layout): смещения от её начала. */
__config_start = /* 0xXXXXXXXX */;
__config_end   = /* 0xXXXXXXXX */;
{%- endif %}
{%- if external_flash == "true" %}

/* Конец последнего раздела на внешней флеши (bsp::layout): микросхема меньше
   не годится. */
__ext_flash_end = {% if ota == "true" and ota_store == "external" and config_store == "external" %}MAX(MAX(__bootloader_state_end, __bootloader_dfu_end), __config_end){% elsif ota == "true" and ota_store == "external" %}MAX(__bootloader_state_end, __bootloader_dfu_end){% else %}__config_end{% endif %};
{%- endif %}

/* Сюда пишет panic-persist — по голым адресам, без секции: секция здесь
   ломает flip-link, см. docs/diagnostics.md. */
_panic_dump_start = ORIGIN(PANIC);
_panic_dump_end   = ORIGIN(PANIC) + LENGTH(PANIC);
