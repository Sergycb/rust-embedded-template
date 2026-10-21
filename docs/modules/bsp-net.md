Сеть платы — Ethernet-устройство (`bsp::net::Device`) под стеком `embassy-net`.
Стек, его память, адресацию и сокеты собирает приложение
(`crates-cross/app/src/net.rs`, сокеты протоколов — `crates-cross/app/src/link.rs`):
число сокетов и порты — решение приложения, а не платы.

Шаблон выводы не выбирает (`docs/architecture.md`), поэтому `Device` пока —
заглушка `bsp::stub::NetDevice`: линк всегда опущен, кадров нет. Всё остальное
настоящее: стек `embassy-net` с DHCP, его фоновая работа в узле `NET`
(`domain::net`) и сокеты протоколов{% if has_tcp == "true" %} (`adapters::net::TcpServer`){% endif %}{% if has_udp == "true" %} (`adapters::net::UdpLink`){% endif %}.
Проект собирается и запускается, протокол ждёт клиента вечно. Подставить
Ethernet — заменить устройство: стек, сокеты и граф не меняются.

Устройство плата отдаёт в `bsp::Drivers::eth`{% if rng_irq != "" %} вместе с генератором случайных чисел
(`Drivers::rng`, затравка стека){% endif %}, а не в `Board`: фоновая работа стека и сам
стек рождаются одним `embassy_net::new` и уходят разным узлам, поэтому пару
один раз собирает `crates-cross/app/src/parts.rs`, а граф делит её по полям
(`parts.net.runner`, `parts.net.stack`).

## Как подставить свой Ethernet

1. Выводы RMII и прерывание — из справочника по чипу:

   ```text
   cargo xtask pins ETH --snippet
   ```

   Он печатает `bind_interrupts!` и группу `assign_resources!`: `REF_CLK`,
   `CRS_DV`, `RXD0`/`RXD1`, `TXD0`/`TXD1`, `TX_EN`, `MDIO`, `MDC`. Выберите те, что
   разведены к PHY на плате (на Nucleo-144 — `LAN8742A`).

2. Псевдоним — на драйвер `embassy-stm32`, сборка — вместо заглушки:

   ```rust,ignore
   use embassy_stm32::eth::{Ethernet, GenericPhy, PacketQueue, Sma};
   use embassy_stm32::peripherals::{ETH, ETH_SMA};

   pub type Device = Ethernet<'static, ETH, GenericPhy<Sma<'static, ETH_SMA>>>;

   // Рядом, статиком модуля: очередь кадров — память блока, а не стека.
   static QUEUE: ConstStaticCell<PacketQueue<4, 4>> = ConstStaticCell::new(PacketQueue::new());

   // В `new`, вместо `Device::new(mac_from_uid(uid))`:
   Ethernet::new(
       QUEUE.take(), r.eth, Irqs,
       r.ref_clk, r.crs, r.rx_d0, r.rx_d1, r.tx_d0, r.tx_d1, r.tx_en,
       mac_from_uid(uid), r.sma, r.mdio, r.mdc,
   )
   ```

   Точные аргументы зависят от версии блока Ethernet чипа (v1 у F4/F7, v2 у
   H5/H7) — сверяйте с `embassy-stm32` 0.6 (`src/eth/v1`, `src/eth/v2`).
   `new` получает группу `eth` из `resources.rs`: `Board::new` уже зовёт
   `crate::net::new(uid, r.eth)` (`docs/resources.md`); выводы добавляются в группу.

3. В графе — тип устройства во фрагменте `NET`: он уже назван через
   `bsp::net::Device`, менять ничего не нужно.

## Адрес, MAC и порты

* Адрес — по DHCP (`Config::dhcpv4` в `crates-cross/app/src/net.rs`). Статический —
  одной строкой там же:
  `Config::ipv4_static(StaticConfigV4 { address: Ipv4Cidr::new(Ipv4Address::new(192, 168, 1, 50), 24), gateway: None, dns_servers: Default::default() })`.
* MAC — из заводского номера чипа (`mac_from_uid` в `bsp/src/net.rs`), локально
  администрируемый. Свой, выданный производителем, — подставьте там же.
* Порты протоколов — `PORT` в подмодулях `crates-cross/app/src/link.rs`: их
  задаёт протокол, а не плата.
* Сокетов в стеке — `SOCKETS` в `crates-cross/app/src/net.rs`: DHCP-клиент и по
  одному на сетевой транспорт. Свой сокет — плюс один.
{%- if has_tcp == "true" %}
* TCP: клиент один, `TcpServer` принимает следующего, когда текущий отключился.
{%- endif %}
{%- if has_udp == "true" %}
* UDP: ответ уходит отправителю последней датаграммы.
{%- endif %}
