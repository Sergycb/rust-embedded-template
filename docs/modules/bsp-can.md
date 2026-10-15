Шина CAN платы — под узлом CANopen.

Шаблон выводы не выбирает (`docs/architecture.md`), поэтому в `Can` пока
стоит заглушка `adapters::can::Unconnected`: проект собирается и
запускается, узел CANopen ждёт кадров вечно, а отправка уходит в никуда.
Прослойка к контроллеру — `Bus`, порт `ports::CanBus` поверх CAN-драйвера
`embassy-stm32`, — уже здесь и компилируется под этот чип; не хватает только
выводов.

## Как подставить свой CAN

1. Блоки для своего контроллера — из справочника по чипу:

   ```text
   cargo xtask pins FDCAN1 --snippet     # или CAN1 — у чипов с bxCAN
   ```

   Он печатает `bind_interrupts!` и группу `assign_resources!` со свободными
   выводами RX/TX; выберите те, что разведены на плате к трансиверу.

2. Псевдоним — на `Bus`, конструктор — на сборку контроллера:

   ```rust,ignore
   pub type Can = Bus;

   // FDCAN (G0, G4, H5, H7, L5, U5):
   pub(crate) fn new(r: Fdcan1Resources) -> Can {
       let mut can = embassy_stm32::can::CanConfigurator::new(r.can, r.rx, r.tx, Irqs);
       can.set_bitrate(250_000);
       Bus::new(can.into_normal_mode())
   }

   // bxCAN (F1, F2, F3, F4, F7, L4):
   pub(crate) fn new(r: Can1Resources) -> Can {
       let mut can = embassy_stm32::can::Can::new(r.can, r.rx, r.tx, Irqs);
       can.set_bitrate(250_000);
       Bus::new(can) // включит контроллер первый обмен
   }
   ```

   В `Board::new` вызов `crate::can::new()` получает свою группу:
   `crate::can::new(r.fdcan1)` после `let r = split_resources!(p);`
   (`docs/resources.md`).

Скорость шины — одна на всю сеть CANopen (обычно 125, 250, 500 или
1000 кбит/с); адрес узла — `node_id` во входе узла в
`crates-cross/app/src/graph.rs`, словарь объектов — `device.toml` в
`crates-host/canopen`.
