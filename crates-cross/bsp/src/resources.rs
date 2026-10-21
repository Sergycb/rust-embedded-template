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
//! флеш{% if graph == "true" %}, сторожевой таймер{% endif %}{% if external_flash == "true" %}, шину внешней флеши{% endif %}{% if has_net == "true" %}{% if rng_irq != "" %}, RNG{% endif %}{% endif %}{% if links != "" %} и блоки выбранных транспортов (группа на транспорт){% endif %}, —
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
{%- if external_flash == "true" %}
    // Шина внешней флеши (`bsp::external`): выводы и DMA — `cargo xtask pins {{ext_flash_block}} --snippet`.
    ext_flash: ExtFlashResources { {% if ext_flash_bus == "spi" %}spi{% elsif ext_flash_bus == "quadspi" %}qspi{% else %}ospi{% endif %}: {{ext_flash_block}} },
{%- endif %}
{%- assign xs = links | split: "," %}
{%- if has_net == "true" %}
{%- if rng_irq != "" %}
    rng: RngResources { rng: RNG },
{%- endif %}
    // Один блок на `tcp` и `udp`: оба транспорта — сокеты одного стека.
    eth: EthResources { eth: ETH },
{%- endif %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] != "tcp" and f[0] != "udp" %}
{%- assign parts = f[4] | split: "_" %}
    {{ f[4] }}: {% for part in parts %}{{ part | capitalize }}{% endfor %}Resources { {% if f[0] == "uart" %}usart{% elsif f[0] == "can" %}can{% else %}usb{% endif %}: {{ f[2] }} },
{%- endif %}
{%- endfor %}
{%- endif %}
}
