UART платы — поток байтов, по подмодулю на транспорт (`bsp::serial::<транспорт>`).
{%- if link_kinds contains "uart" %}
Канал протокола над ним ({% if has_modbus == "true" %}Modbus RTU{% else %}protobuf с кадрами COBS{% endif %}) собирает узел
протокола (`ports::Assemble`), а тип канала и его буферы задаёт приложение
(`crates-cross/app/src/link/uart.rs`).
{%- endif %}

Шаблон выводы не выбирает (`docs/architecture.md`), поэтому в `Serial` пока
стоит заглушка `bsp::stub::Serial`: проект собирается и
запускается, {% if link_kinds contains "uart" %}узел протокола{% else %}приём{% endif %} ждёт байтов вечно, а запись уходит в никуда.
Поток лежит полем `Board::<транспорт>` (например `usart1`), а какой это USART и на
каких ножках — решает только этот модуль, поэтому заменить заглушку на драйвер —
правка в одном месте.

## Как подставить свой UART

1. Блоки для своего USART — из справочника по чипу (блок транспорта, например
   `{{iface_periph}}`):

   ```text
   cargo xtask pins {{iface_periph}} --snippet
   ```

   Он печатает `bind_interrupts!` и группу `assign_resources!` со свободными
   выводами; выберите те, что разведены на плате.

2. В подмодуле транспорта — псевдоним на драйвер `embassy-stm32` (задачи
   embassy не generic, и узлу нужен именованный тип), конструктор — на сборку
   драйвера. Буферы `BufferedUart` — память драйвера, а не протокола, поэтому
   они здесь, в плате:

   ```rust,ignore
   use embassy_stm32::usart::{BufferedUart, Config};
   use crate::resources::Usart1Resources;
   use static_cell::ConstStaticCell;

   pub type Serial = BufferedUart<'static>;

   static TX: ConstStaticCell<[u8; 256]> = ConstStaticCell::new([0; 256]);
   static RX: ConstStaticCell<[u8; 256]> = ConstStaticCell::new([0; 256]);

   pub(crate) fn new(r: Usart1Resources) -> Serial {
       let mut config = Config::default();
       config.baudrate = 115_200;
       BufferedUart::new(r.usart, r.rx, r.tx, TX.take(), RX.take(), Irqs, config)
           .expect("конфигурация UART")
   }
   ```

   `Board::new` уже передаёт группу транспорта из `resources.rs`:
   `crate::serial::<транспорт>::new(r.<транспорт>)` (`docs/resources.md`); выводы
   добавляются в неё.

`BufferedUart` реализует `embedded_io_async::{Read, ReadReady, Write}` сам{% if link_kinds contains "uart" %} —
ровно то, что ждёт транспорт {% if has_modbus == "true" %}`adapters::modbus::Rtu`{% else %}`adapters::cobs::Cobs`{% endif %}{% endif %},
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

Версию USART показывает тот же `cargo xtask pins {{iface_periph}}`.
