//! Распиновка платы: выводы, периферия и DMA-каналы, разложенные по
//! именованным группам (docs/resources.md).
//!
//! Вся периферия чипа разбирается здесь, а не в `Board::new` вразнобой:
//! `let r = split_resources!(p);` раскладывает `Peripherals` по группам, и
//! каждая группа уходит в конструктор своего модуля. Наружу из `bsp` группы не
//! выходят.
{%- if has_resources != "true" %} Заготовку группы
//! под блок с реальными именами выводов печатает
//! `cargo xtask pins <БЛОК> --snippet`; переносится в макрос ниже только сама
//! группа.
{%- else %} Шаблон кладёт сюда блоки того, что нужно ему самому:
//! флеш{% if graph == "true" %}, сторожевой таймер{% endif %}{% if interface == "tcp" or interface == "udp" %}{% if rng_irq != "" %}, RNG{% endif %}{% endif %}{% if interface != "none" %} и блок выбранного интерфейса{% endif %}, —
//! без выводов: их выбирает плата. Заготовку с выводами и DMA-каналами печатает
//! `cargo xtask pins <БЛОК> --snippet`; из неё в группу переносятся поля.
{%- endif %} Занят ли отладочный порт —
//! `cargo xtask pins --check`.

use assign_resources::assign_resources;
{%- if has_resources != "true" %}
// Макрос раскрывается в `Peri<'static, peripherals::X>` и оба имени берёт
// отсюда. Пока группа пустая, они не нужны — `expect` сам напомнит снять его,
// когда в группе появится первый вывод.
#[expect(unused_imports, reason = "группа пока пустая — см. комментарий выше")]
{%- endif %}
use embassy_stm32::{Peri, peripherals};

assign_resources! {
{%- if has_resources != "true" %}
    // Пример: `sensor: SensorResources { i2c: I2C1, scl: PB6, sda: PB7 },`
    board: BoardResources {}
{%- else %}
{%- if shared_flash == "true" %}
    flash: FlashResources { flash: FLASH },
{%- endif %}
{%- if graph == "true" %}
    watchdog: WatchdogResources { iwdg: {{watchdog_peripheral}} },
{%- endif %}
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
