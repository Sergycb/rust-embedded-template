# Распиновка платы

Именованные группы ресурсов платы (пины, периферия, DMA-каналы),
собранные через [`assign_resources!`](https://docs.rs/assign-resources).

Группы нужны самому `bsp`: из них он собирает драйверы. Наружу, в `app`,
уходит не периферия, а объект платы — поле `Board` под трейтом
(`embedded-hal`, `embedded-io-async`, порт из `ports`, где стандартного трейта
нет). Приложение про пины и DMA не знает вовсе, а адаптеры над объектом
(драйвер датчика, транспорт протокола) собирает само: см. docs/architecture.md,
«`Board` отдаёт объекты платы, а не периферию и не адаптеры».

Распиновка живёт в `crates-cross/bsp/src/resources.rs`.
{%- if interface == "none" %} Там уже стоит
`assign_resources!` с одной пустой группой `board` — пустой макрос не
компилируется, ему нужна хотя бы одна.
{%- else %} Там уже стоят группы транспортов, выбранных при генерации, по
группе на транспорт с его именем:
{%- assign xs = links | split: "," %}
{%- assign sep = "" %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "uart" or f[0] == "can" or f[0] == "usb" %}{{ sep }} `{{ f[4] }}` (`{{ f[2] }}`, конструктор `crate::{% if f[0] == "uart" %}serial{% else %}{{ f[0] }}{% endif %}::{{ f[4] }}::new`)
{%- assign sep = "," %}
{%- endif %}
{%- endfor %}
{%- if has_net == "true" %}{{ sep }} `eth` (Ethernet, `crate::net::new`){% if rng_irq != "" %} и `rng` (`RNG`, затравка стека — `crate::rng::new`){% endif %}
{%- endif %}. В группах только блоки, без выводов и DMA-каналов: их выбирает плата, добавьте
их в группу, а конструктор модуля уже принимает свою группу параметром —
заглушка её не трогает.
{%- endif %} Модуль подключён в `lib.rs` первым и с
`#[macro_use]`, иначе `split_resources!` не был бы виден из `board.rs`.

Имена полей `embassy_stm32::Peripherals` зависят от выбранного чипа
(`{{chip_feature}}`), поэтому шаблон не может зашить готовую распиновку —
добавьте свои группы в макрос (пустую `board` можно заменить или заполнить)
и снимите `#[expect(unused_imports)]` с импорта `Peri`/`peripherals`, когда
появится первый вывод.
Подсмотреть, какие выводы умеют нужный сигнал (и не заняты ли они
отладочным портом или кварцем), — `cargo xtask pins I2C1`; готовую
заготовку с правильными типами обработчиков прерываний печатает
`cargo xtask pins I2C1 --snippet` — из её половины `assign_resources!` в
`resources.rs` переносится только группа: второй вызов макроса в крейте
не компилируется (`split_resources` объявлен дважды).

```ignore
assign_resources! {
    sensor: SensorResources {
        i2c: I2C1,
        scl: PB6,
        sda: PB7,
        tx_dma: DMA1_CH6,
        rx_dma: DMA1_CH7,
    }
}
```

Дальше — в `Board::new` (board.rs), там же, где создаётся общий `Flash`. Плата
собирает шину, а не драйвер датчика:

```ignore
// Макрос раскрывается в `AssignedResources { .. }` — типы групп нужны в
// области видимости вызова.
use crate::resources::*;

let r = split_resources!(p);
let sensor_bus = embassy_stm32::i2c::I2c::new(
    r.sensor.i2c, r.sensor.scl, r.sensor.sda,
    Irqs, r.sensor.tx_dma, r.sensor.rx_dma,
    khz(100), Default::default(),
);
```

и полем в `Board`: `pub sensor_bus: SensorBus`, где
`pub type SensorBus = I2c<'static, Async>` — псевдоним, потому что задачи embassy
не могут быть generic. Частота шины — свойство платы, поэтому она здесь.

Драйвер датчика над шиной — адаптер: generic по трейтам `embedded-hal`, живёт в
`crates-host/adapters` (его проверяет host-тест с моком, а не плата) и реализует
порт из `ports`, которым пользуется логика `domain`. Собирает его приложение —
в выражении входа узла в графе или в модуле задачи без графа:
`adapters::YourDriver::new(parts.board.sensor_bus, YOUR_ADDRESS)`. Буферы и
настройки драйвера (адрес, режим) — там же: это решение приложения, а не платы.

**Вся периферия разбирается здесь.** Шаблон сам кладёт в группы то, что ему
нужно: `FLASH` (группа `flash`, если есть общий флеш){% if graph == "true" %}, `{{watchdog_peripheral}}` (группа `watchdog`, сторож){% endif %}
и блок выбранного интерфейса. Блок, отданный группе, из `Peripherals` ушёл —
второй раз в другую группу его не положить, компилятор не даст.
