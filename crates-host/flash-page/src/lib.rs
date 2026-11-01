#![cfg_attr(not(test), no_std)]

//! Геометрия флеша под разделами.
// {%- if erase_page == "true" %}
//! [`ErasePage`] — обёртка со своей страницей стирания.
// {%- endif %}
// {%- if ota_store == "external" %}
//! [`erase_size`] — для проверок раскладки при компиляции.
// {%- endif %}
//!
//! Это не адаптер порта, а свойство раскладки: страницу раздела знает генерация
// {%- if ota == "true" %}
//! (`chip-data-gen`), а пользуются обёрткой `bsp` (разделы приложения) и
//! `boot` (те же разделы в bootloader'е). Отдельным крейтом, а не модулем `bsp`,
//! потому что `bsp` собирается только под target, а обмен `embassy-boot` над
//! страницей крупнее физической проверяется здесь, на хосте (`coarse_swap`).
// {%- else %}
//! (`chip-data-gen`), а пользуется обёрткой `bsp`. Отдельным крейтом, а не
//! модулем `bsp`, потому что `bsp` собирается только под target, а обёртка
//! проверяется здесь, на хосте.
// {%- endif %}
//!
// {%- if ota == "true" and config == "true" %}
//! Флеш на чипе один, а `embassy-boot` и `sequential-storage` хотят каждый свой
// {%- elsif ota == "true" %}
//! Флеш на чипе один, а разделы `embassy-boot` хотят каждый свой
// {%- else %}
//! Флеш на чипе один, а `sequential-storage` хочет свой
// {%- endif %}
//! `NorFlash` по значению. Общий флеш лежит под блокирующим мьютексом, а
//! каждый потребитель получает `BlockingPartition` из `embassy-embedded-hal` —
//! ссылку на него: блокировка берётся на каждую операцию отдельно. Асинхронный
//! вид даёт `embassy_embedded_hal::adapter::BlockingAsync`; операции
//! блокирующие, ни одна не ждёт внутри. Обёртка generic по флешу, а не
//! привязана к `embassy_stm32::flash::Flash`: ей всё равно, что под ней.

mod page;

// {%- if erase_page == "true" %}
pub use page::ErasePage;
// {%- endif %}
// {%- if ota_store == "external" %}
pub use page::erase_size;
// {%- endif %}
