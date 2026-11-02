# Распиновка платы

Именованные группы ресурсов платы (пины, периферия, DMA-каналы),
собранные через [`assign_resources!`](https://docs.rs/assign-resources).

Группы нужны самому `bsp`: из них он собирает драйверы. Наружу, в `app`,
уходит не периферия, а рычаг платы — поле `Board` под трейтом:
docs/architecture.md, «`Board` отдаёт рычаги платы, а не периферию и не протоколы».

Распиновка живёт в `crates-cross/bsp/src/resources.rs`.
{%- if has_resources != "true" %} Там уже стоит
`assign_resources!` с одной пустой группой `board` — пустой макрос не
компилируется, ему нужна хотя бы одна.
{%- else %} Там уже стоят группы, которые шаблон кладёт сам (ниже, «Вся
периферия разбирается здесь»).
{%- if links != "" %} Транспортам, выбранным при генерации, — по
группе на транспорт с его именем:
{%- assign xs = links | split: "," %}
{%- assign sep = "" %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "uart" or f[0] == "can" or f[0] == "usb" %}{{ sep }} `{{ f[4] }}` (`{{ f[2] }}`, конструктор `crate::{% if f[0] == "uart" %}serial{% else %}{{ f[0] }}{% endif %}::{{ f[4] }}::new`)
{%- assign sep = "," %}
{%- endif %}
{%- endfor %}
{%- if has_net == "true" %}{{ sep }} `eth` (Ethernet, `crate::net::new`){% if rng_irq != "" %} и `rng` (`RNG`, затравка стека — `crate::rng::seed`){% endif %}
{%- endif %}. В группах только блоки, без выводов и DMA-каналов: их выбирает плата, добавьте
их в группу, а конструктор модуля уже принимает свою группу параметром —
заглушка её не трогает.
{%- endif %}
{%- endif %} Модуль подключён в `lib.rs` первым и с
`#[macro_use]`, иначе `split_resources!` не был бы виден из `board.rs`.

Имена полей `embassy_stm32::Peripherals` зависят от выбранного чипа
(`{{chip_feature}}`), поэтому шаблон не может зашить готовую распиновку —
добавьте свои группы в макрос{% if has_resources != "true" %} (пустую `board` можно заменить или заполнить)
и снимите `#[expect(unused_imports)]` с импорта `Peri`/`peripherals`, когда
появится первый вывод{% endif %}.
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

Дальше — в `Board::new` (board.rs){% if shared_flash == "true" %}, там же, где создаётся общий `Flash`{% endif %}. Сначала
плата собирает шину (драйвер датчика над ней — ниже){% if has_resources == "true" %}; `use crate::resources::*;`
и `let r = split_resources!(p);` там уже стоят — добавьте только сборку шины{% endif %}:

```ignore
{% if has_resources != "true" %}// Макрос раскрывается в `AssignedResources { .. }` — типы групп нужны в
// области видимости вызова.
use crate::resources::*;

let r = split_resources!(p);
{% endif %}let mut config = embassy_stm32::i2c::Config::default();
config.frequency = embassy_stm32::time::Hertz::khz(100);
let sensor_bus = embassy_stm32::i2c::I2c::new(
    r.sensor.i2c, r.sensor.scl, r.sensor.sda,
    r.sensor.tx_dma, r.sensor.rx_dma,
    Irqs, config,
);
```

и полем в `Board`: `pub sensor_bus: SensorBus`, где
`pub type SensorBus = I2c<'static, Async, Master>` (в `board.rs`, рядом с
`Board`; наружу — `pub use` в `lib.rs`, тогда задача назовёт его
`bsp::SensorBus`) — псевдоним, потому что задачи embassy не могут быть
generic. `Irqs` — `bind_interrupts!` с обработчиками I2C (заготовку печатает
тот же `--snippet`) и обоих DMA-каналов (`dma::InterruptHandler` — их
допишите сами). Частота шины — свойство платы, поэтому она здесь.

Драйвер датчика — тоже рычаг платы: его объект собирает `bsp` над шиной и
отдаёт полем `Board`, а тип драйвера, generic по трейтам `embedded-hal`, живёт
в хостовом крейте или во внешнем{% if external_flash == "true" %}, как `w25q` под внешней
флешью{% endif %}. Заводить ли над ним трейт — docs/architecture.md, «Трейт — только
под несколько реализаций».
{%- capture own %}{% if shared_flash == "true" %}, `FLASH` (группа `flash`){% endif %}{% if graph == "true" %}, `{{watchdog_peripheral}}` (группа `watchdog`, сторож){% endif %}{% if external_flash == "true" %}, `{{ext_flash_block}}` (группа `ext_flash`, шина внешней флеши){% endif %}{% if has_net == "true" and rng_irq != "" %}, `RNG` (группа `rng`){% endif %}{% if links != "" %}, блоки выбранных транспортов{% endif %}{% endcapture %}

**Вся периферия разбирается здесь.**{% if own != "" %} Шаблон сам кладёт в группы то, что ему
нужно: {{ own | remove_first: ", " }}.{% endif %} Блок, отданный группе, из `Peripherals` ушёл —
второй раз в другую группу его не положить, компилятор не даст.
