Сеть платы — стек `embassy-net` под протоколом поверх {% if interface == "tcp" %}TCP{% else %}UDP{% endif %}.

Шаблон выводы не выбирает (`docs/architecture.md`), поэтому сетевое
устройство `Device` пока — заглушка `adapters::net::Unconnected`: линк
всегда опущен, кадров нет. Всё остальное настоящее: стек `embassy-net` с
DHCP, его фоновая работа в узле `NET` графа (`domain::net`) и сокет протокола
(`adapters::net::{% if interface == "tcp" %}TcpServer{% else %}UdpLink{% endif %}` на порту `PORT`; сокет и порт — в `crates-cross/app/src/graph.rs`, `bsp` отдаёт
стек). Проект собирается и
запускается, протокол ждёт {% if interface == "tcp" %}клиента{% else %}датаграмм{% endif %} вечно. Подставить Ethernet — заменить
устройство: стек, сокет и граф не меняются.

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

   // Рядом с остальными статиками модуля:
   static QUEUE: ConstStaticCell<PacketQueue<4, 4>> = ConstStaticCell::new(PacketQueue::new());

   // В `new`, вместо `Unconnected::new(adapters::net::mac_from_uid(uid))`:
   let device = Ethernet::new(
       QUEUE.take(), r.eth, Irqs,
       r.ref_clk, r.crs, r.rx_d0, r.rx_d1, r.tx_d0, r.tx_d1, r.tx_en,
       adapters::net::mac_from_uid(uid), r.sma, r.mdio, r.mdc,
   );
   ```

   Точные аргументы зависят от версии блока Ethernet чипа (v1 у F4/F7, v2 у
   H5/H7) — сверяйте с `embassy-stm32` 0.6 (`src/eth/v1`, `src/eth/v2`).
   `new` получает свою группу ресурсов: `crate::net::new(uid, seed, r.eth)` после
   `let r = split_resources!(p);` (`docs/resources.md`).

3. В графе — тип устройства во фрагменте `NET`: он уже назван через
   `bsp::net::Device`, менять ничего не нужно.

## Адрес и порт

* Адрес — по DHCP (`Config::dhcpv4`). Статический — одной строкой:
  `Config::ipv4_static(StaticConfigV4 { address: Ipv4Cidr::new(Ipv4Address::new(192, 168, 1, 50), 24), gateway: None, dns_servers: Default::default() })`.
* MAC — из заводского номера чипа (`adapters::net::mac_from_uid`), локально администрируемый.
  Свой, выданный производителем, — подставьте там же.
* Порт протокола — `PORT` в `crates-cross/app/src/graph.rs`: его задаёт протокол,
  а не плата.
{%- if interface == "tcp" %}
* Клиент один: `TcpServer` принимает следующего, когда текущий отключился.
{%- else %}
* Ответ уходит отправителю последней датаграммы.
{%- endif %}
