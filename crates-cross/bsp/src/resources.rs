//! Распиновка платы: выводы, периферия и DMA-каналы, разложенные по
//! именованным группам (docs/resources.md).
//!
//! Группы нужны самому `bsp` — из них `Board::new` собирает драйверы
//! (`let r = split_resources!(p);`), — и наружу не выходят.
{%- if interface == "none" %} Заготовку группы
//! под блок с реальными именами выводов печатает
//! `cargo xtask pins <БЛОК> --snippet`; переносится в макрос ниже только сама
//! группа.
{%- else %} Шаблон кладёт сюда блоки того, что выбрано при генерации{% if rng_irq != "" and interface == "tcp" or rng_irq != "" and interface == "udp" %} (интерфейс и RNG){% else %} (интерфейс){% endif %}, —
//! без выводов: их выбирает плата. Заготовку с выводами и DMA-каналами печатает
//! `cargo xtask pins <БЛОК> --snippet`; из неё в группу переносятся поля.
{%- endif %} Занят ли отладочный порт —
//! `cargo xtask pins --check`.
//!
//! `FLASH`{% if graph == "true" %} и `{{watchdog_peripheral}}`{% endif %} в группы не включайте: их забирает `Board::new`.

use assign_resources::assign_resources;
{%- if interface == "none" %}
// Макрос раскрывается в `Peri<'static, peripherals::X>` и оба имени берёт
// отсюда. Пока группа пустая, они не нужны — `expect` сам напомнит снять его,
// когда в группе появится первый вывод.
#[expect(unused_imports, reason = "группа пока пустая — см. комментарий выше")]
{%- endif %}
use embassy_stm32::{Peri, peripherals};

assign_resources! {
{%- if interface == "none" %}
    // Пример: `sensor: SensorResources { i2c: I2C1, scl: PB6, sda: PB7 },`
    board: BoardResources {}
{%- else %}
{%- if interface == "tcp" or interface == "udp" %}
{%- if rng_irq != "" %}
    rng: RngResources { rng: RNG },
{%- endif %}
    eth: EthResources { eth: {{iface_periph}} },
{%- elsif interface == "uart" %}
    serial: SerialResources { usart: {{iface_periph}} },
{%- elsif interface == "can" %}
    can: CanResources { can: {{iface_periph}} },
{%- elsif interface == "usb" %}
    usb: UsbResources { usb: {{iface_periph}} },
{%- endif %}
{%- endif %}
}
