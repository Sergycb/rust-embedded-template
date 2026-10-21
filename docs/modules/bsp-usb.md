USB платы — драйвер USB-блока, по подмодулю на транспорт (`bsp::usb::<транспорт>`).
Устройство `embassy-usb` с классом CDC ACM («виртуальный COM-порт») над ним
собирает приложение (`crates-cross/app/src/usb.rs`): идентификаторы, классы и
их память — решение приложения, а не платы.

Шаблон выводы не выбирает (`docs/architecture.md`), поэтому `UsbDriver` пока —
заглушка `bsp::stub::UsbDriver`: шина никогда не просыпается, хост устройство не
видит. Всё остальное настоящее: устройство `embassy-usb` с классом CDC, его
фоновая работа в узле `USB_<транспорт>` (`domain::usb`) и поток
`adapters::usb::CdcStream` под протоколом или вашим узлом. Проект собирается и
запускается, протокол ждёт хоста вечно. Подставить USB-блок — заменить драйвер:
устройство, класс и граф не меняются.

Драйвер плата отдаёт в `bsp::Drivers`, а не в `Board`: устройство и поток CDC
рождаются одним `Builder` и уходят разным узлам, поэтому пару один раз собирает
`crates-cross/app/src/parts.rs`, а граф делит её по полям
(`parts.<транспорт>.device`, `parts.<транспорт>.cdc`).

## Как подставить свой USB

1. Выводы и прерывание — из справочника по чипу (блок транспорта, например
   `{{iface_periph}}`):

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

2. В подмодуле транспорта — псевдоним на драйвер `embassy-stm32` и сборка
   вместо заглушки:

   ```rust,ignore
   use embassy_stm32::usb::Driver;
   use embassy_stm32::peripherals::{{iface_periph}};

   pub type UsbDriver = Driver<'static, {{iface_periph}}>;

{%- if usb_kind == "otg" %}
   // Рядом, статиком модуля: буфер приёма драйвера — память блока, а не класса.
   static EP_OUT: ConstStaticCell<[u8; 256]> = ConstStaticCell::new([0; 256]);

   // В `new`, вместо `UsbDriver::default()`:
   let mut otg = embassy_stm32::usb::Config::default();
   otg.vbus_detection = false; // VBUS не разведён — иначе true
   Driver::new_fs(r.usb, Irqs, r.dp, r.dm, EP_OUT.take(), otg)
{%- else %}

   // В `new`, вместо `UsbDriver::default()`:
   Driver::new(r.usb, Irqs, r.dp, r.dm)
{%- endif %}
   ```

   `new` получает группу ресурсов транспорта из `resources.rs`: `Board::new` уже
   зовёт `crate::usb::<транспорт>::new(r.<транспорт>)` (`docs/resources.md`);
   выводы добавляются в группу.

## Идентификаторы

`VID`/`PID` — в `crates-cross/app/src/usb.rs`: пара из примеров `embassy-usb`
(`0xC0DE:0xCAFE`), не зарегистрированная, для изделия нужны свои. Серийный
номер — заводской номер чипа (`ChipSpec::uid_hex`), у второго и следующих
устройств одной платы — с суффиксом номера: при общих VID/PID хост различает
устройства по нему.
