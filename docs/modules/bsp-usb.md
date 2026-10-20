USB платы — устройство `embassy-usb` с классом CDC ACM («виртуальный
COM-порт») под протоколом: {% if protocol == "modbus" %}Modbus RTU{% else %}protobuf с кадрами COBS{% endif %}.

Шаблон выводы не выбирает (`docs/architecture.md`), поэтому USB-драйвер
`UsbDriver` пока — заглушка `bsp::stub::UsbDriver`: шина никогда не
просыпается, хост устройство не видит. Всё остальное настоящее: устройство
`embassy-usb` с классом CDC, его фоновая работа в узле `USB` графа
(`domain::usb`) и поток `adapters::usb::CdcStream` под {% if protocol == "modbus" %}кадрами RTU{% else %}кадрами COBS{% endif %}. Проект
собирается и запускается, протокол ждёт хоста вечно. Подставить USB-блок —
заменить драйвер: устройство, класс и граф не меняются.

## Как подставить свой USB

1. Выводы и прерывание — из справочника по чипу:

   ```text
   cargo xtask pins {{iface_periph}} --snippet
   ```
{%- if usb_kind == "otg" and iface_periph != "USB_OTG_FS" %}

   У этого чипа выведен только `{{iface_periph}}` (так бывает у H723, H730,
   H7A3): ниже `USB_OTG_FS` читайте как `{{iface_periph}}`, а `Driver::new_fs` — как
   конструктор для этого блока.
{%- endif %}

   Он печатает `bind_interrupts!` и группу `assign_resources!` с выводами
   `DP`/`DM`. USB тактируется частотой 48 мегагерц (`HSI48` или PLL): без неё хост
   устройство не увидит (`docs/clocks.md`).

2. Псевдоним — на драйвер `embassy-stm32`, сборка — вместо заглушки:

   ```rust,ignore
   use embassy_stm32::usb::Driver;
   use embassy_stm32::peripherals::{{iface_periph}};

   pub type UsbDriver = Driver<'static, {{iface_periph}}>;

{%- if usb_kind == "otg" %}
   // Рядом с остальными статиками модуля:
   static EP_OUT: ConstStaticCell<[u8; 256]> = ConstStaticCell::new([0; 256]);

   // В `new`, вместо `Unconnected::default()`:
   let mut otg = embassy_stm32::usb::Config::default();
   otg.vbus_detection = false; // VBUS не разведён — иначе true
   let driver = Driver::new_fs(r.usb, Irqs, r.dp, r.dm, EP_OUT.take(), otg);
{%- else %}

   // В `new`, вместо `Unconnected::default()`:
   let driver = Driver::new(r.usb, Irqs, r.dp, r.dm);
{%- endif %}
   ```

   `new` получает свою группу ресурсов, `usb` в `resources.rs`: `Board::new` уже
   зовёт `crate::usb::new(r.usb)` (`docs/resources.md`); выводы добавляются в группу.

## Идентификаторы

`VID`/`PID` — пара из примеров `embassy-usb` (`0xC0DE:0xCAFE`), не
зарегистрированная: для изделия нужны свои. Серийный номер — заводской
номер чипа, у каждой платы свой.
