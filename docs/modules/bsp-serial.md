UART платы — поток байтов под узлом протокола (Modbus RTU, protobuf с
кадрами COBS).

Шаблон выводы не выбирает (`docs/architecture.md`), поэтому в `Serial` пока
стоит заглушка `adapters::serial::Unconnected`: проект собирается и
запускается, узел протокола ждёт байтов вечно, а запись уходит в никуда.
Узел получает поток через `Board::serial`, а какой это USART и на каких
ножках — решает только этот модуль, поэтому заменить заглушку на драйвер —
правка в одном месте.

## Как подставить свой UART

1. Блоки для своего USART — из справочника по чипу:

   ```text
   cargo xtask pins USART2 --snippet
   ```

   Он печатает `bind_interrupts!` и группу `assign_resources!` со свободными
   выводами; выберите те, что разведены на плате.

2. Псевдоним — на драйвер `embassy-stm32` (задачи embassy не generic, и
   узлу нужен именованный тип), конструктор — на сборку драйвера:

   ```rust,ignore
   use embassy_stm32::usart::{BufferedUart, Config};
   use static_cell::StaticCell;

   pub type Serial = BufferedUart<'static>;

   pub(crate) fn new(r: Usart2Resources) -> Serial {
       static TX: StaticCell<[u8; 256]> = StaticCell::new();
       static RX: StaticCell<[u8; 256]> = StaticCell::new();
       let mut config = Config::default();
       config.baudrate = 115_200;
       BufferedUart::new(r.usart, r.rx, r.tx, TX.init([0; 256]), RX.init([0; 256]), Irqs, config)
           .expect("конфигурация UART")
   }
   ```

   В `Board::new` вызов `crate::serial::new()` получает свою группу:
   `crate::serial::new(r.usart2)` после `let r = split_resources!(p);`
   (`docs/resources.md`).

`BufferedUart` реализует `embedded_io_async::{Read, ReadReady, Write}` сам —
ровно то, что ждут транспорты `adapters::modbus::Rtu` и `adapters::cobs::Cobs`,
поэтому ни прослойки, ни трейтов в `bsp` не нужно.

## RS-485

RS-485 — тот же UART плюс вывод DE, который включает передатчик линии на
время отправки:

* у USART версий v3/v4 (G0, G4, L4, H7, U5 и др.) DE переключает само железо:
  `BufferedUart::new_with_de(r.usart, r.rx, r.tx, r.de, Irqs, TX, RX, config)`
  — вывод DE берите из той же распечатки `pins` (сигнал `DE`);
* у USART v1/v2 (F1, F2, F4) аппаратного DE нет, и `new_with_de` не
  существует. Переключать DE придётся программно: поднять перед записью,
  опустить после `flush`, — обёрткой поверх `OutputPin`. В шаблоне её нет,
  добавить её в `adapters` — по первой нужде.

Версию USART показывает тот же `cargo xtask pins USART2`.
