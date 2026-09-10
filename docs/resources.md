# Распиновка платы

Именованные группы ресурсов платы (пины, периферия, DMA-каналы),
собранные через [`assign_resources!`](https://docs.rs/assign-resources).

Группы нужны самому `bsp`: из них он собирает драйверы. Наружу, в `app`,
уходит не периферия, а готовый объект — поле `Board`, реализующее порт из
`ports`. Приложение про пины, шины и DMA не знает вовсе, и это правило, а
не текущее состояние: см. docs/architecture.md, «`Board` отдаёт объекты, а не
периферию».

Отдельного модуля под это в шаблоне нет: распиновка живёт там же, где из неё
собираются драйверы, — в `crates-cross/bsp/src/board.rs`. Пустой файл
`resources.rs`, который раньше стоял под неё, был модулем без кода, а
`cargo xtask pins --check` теперь читает весь `crates-cross/bsp/src`, так что
объявить группы можно и отдельным модулем, если их станет много.

Имена полей `embassy_stm32::Peripherals` зависят от выбранного чипа
(`{{chip_feature}}`), поэтому шаблон не может зашить готовую распиновку —
вставьте пример ниже в `board.rs` и замените поля на выводы вашей платы.
Подсмотреть, какие выводы умеют нужный сигнал (и не заняты ли они
отладочным портом или кварцем), — `cargo xtask pins I2C1`; готовую
заготовку с правильными типами обработчиков прерываний печатает
`cargo xtask pins I2C1 --snippet`.

```ignore
use assign_resources::assign_resources;
// `Peri` здесь обязателен: макрос раскрывается в
// `Peri<'static, peripherals::X>` и оба имени берёт из области видимости
// вызова. Без него — `error[E0425]: cannot find type Peri`.
use embassy_stm32::{Peri, peripherals};

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
let r = split_resources!(p);
let i2c = embassy_stm32::i2c::I2c::new(
    r.sensor.i2c, r.sensor.scl, r.sensor.sda,
    Irqs, r.sensor.tx_dma, r.sensor.rx_dma,
    khz(100), Default::default(),
);
// Свой драйвер, generic по трейтам embedded-hal, живёт в domain/adapters:
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
