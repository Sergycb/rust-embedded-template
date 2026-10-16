# Распиновка платы

Именованные группы ресурсов платы (пины, периферия, DMA-каналы),
собранные через [`assign_resources!`](https://docs.rs/assign-resources).

Группы нужны самому `bsp`: из них он собирает драйверы. Наружу, в `app`,
уходит не периферия, а готовый объект — поле `Board`, реализующее порт из
`ports`. Приложение про пины, шины и DMA не знает вовсе, и это правило, а
не текущее состояние: см. docs/architecture.md, «`Board` отдаёт объекты, а не
периферию».

Распиновка живёт в `crates-cross/bsp/src/resources.rs`: там уже стоит
`assign_resources!` с одной пустой группой `board` — пустой макрос не
компилируется, ему нужна хотя бы одна. Модуль подключён в `lib.rs` первым и с
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

Дальше — в `Board::new` (board.rs), там же, где создаётся общий `Flash`:

```ignore
// Макрос раскрывается в `AssignedResources { .. }` — типы групп нужны в
// области видимости вызова.
use crate::resources::*;

let r = split_resources!(p);
let i2c = embassy_stm32::i2c::I2c::new(
    r.sensor.i2c, r.sensor.scl, r.sensor.sda,
    Irqs, r.sensor.tx_dma, r.sensor.rx_dma,
    khz(100), Default::default(),
);
// Свой драйвер, generic по трейтам embedded-hal, живёт в crates-host/adapters:
// его проверяет host-тест с моком, а не плата. Крейт уже объявлен
// зависимостью bsp — правки манифеста не нужно.
let sensor = adapters::YourDriver::new(i2c, YOUR_ADDRESS);
```

и полем в `Board`: `pub sensor: adapters::YourDriver<I2c<'static, Async>>`.
Приложение работает с ним через собственный порт из `ports` — конкретный
тип ему знать незачем (см. docs/architecture.md). Если объект уезжает в
задачу, дайте типу псевдоним (`pub type Sensor = ...`) и называйте в
сигнатуре задачи его: задачи embassy не могут быть generic.

**`FLASH` в группы не включайте.** Его забирает сам `Board::new` до вызова
`split_resources!`, и это законное частичное перемещение ровно до тех пор,
пока макрос к этому полю не обращается.
{%- if graph == "true" %}

**`{{watchdog_peripheral}}` — тоже не включайте, по той же причине:** его
`Board::new` забирает под сторожевой таймер (см. `wdg.rs`), и распиновке
он не принадлежит.
{%- endif %}
