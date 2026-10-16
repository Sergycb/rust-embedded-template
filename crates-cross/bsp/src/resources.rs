//! Распиновка платы: выводы, периферия и DMA-каналы, разложенные по
//! именованным группам (docs/resources.md).
//!
//! Группы нужны самому `bsp` — из них `Board::new` собирает драйверы
//! (`let r = split_resources!(p);`), — и наружу не выходят. Заготовку группы
//! под блок с реальными именами выводов печатает
//! `cargo xtask pins <БЛОК> --snippet`; переносится в макрос ниже только сама
//! группа. Занят ли отладочный порт — `cargo xtask pins --check`.
//!
//! `FLASH`{% if graph == "true" %} и `{{watchdog_peripheral}}`{% endif %} в группы не включайте: их забирает `Board::new`.

use assign_resources::assign_resources;
// Макрос раскрывается в `Peri<'static, peripherals::X>` и оба имени берёт
// отсюда. Пока группа пустая, они не нужны — `expect` сам напомнит снять его,
// когда в группе появится первый вывод.
#[expect(unused_imports, reason = "группа пока пустая — см. комментарий выше")]
use embassy_stm32::{Peri, peripherals};

assign_resources! {
    // Пример: `sensor: SensorResources { i2c: I2C1, scl: PB6, sda: PB7 },`
    board: BoardResources {}
}
