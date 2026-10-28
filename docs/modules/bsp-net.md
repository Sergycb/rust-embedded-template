Сеть платы — Ethernet-устройство (`bsp::net::Device`) и стек `embassy-net` над
ним: `board.net` (`bsp::net::Net`) — фоновая работа стека (`runner`) и сам стек
(`stack`). Стек, его память и адресацию собирает плата; сокеты на нём открывают
узлы — каналы протоколов и соединения служб собирает каждый узел сам
(`ports::Assemble`), а память сокетов и порты заводит приложение
{%- assign sep = " (" %}
{%- if link_kinds contains "tcp" or link_kinds contains "udp" %}{{ sep }}`crates-cross/app/src/link/`{% assign sep = ", " %}{% endif %}
{%- if net_services != "" %}{{ sep }}`crates-cross/app/src/net/`{% assign sep = ", " %}{% endif %}
{%- if sep == ", " %}){% endif %}: порты — решение
протокола, а не платы.

Шаблон выводы не выбирает (`docs/architecture.md`), поэтому `Device` пока —
заглушка `bsp::stub::NetDevice`: линк всегда опущен, кадров нет. Всё остальное
настоящее: стек `embassy-net` с DHCP, его фоновая работа в узле `NET`
(`domain::net`) и сокеты протоколов{% if has_tcp == "true" %} (`adapters::net::TcpServer`){% endif %}{% if has_udp == "true" %} (`adapters::net::UdpLink`){% endif %}.
Проект собирается и запускается, протокол ждёт клиента вечно. Подставить
Ethernet — заменить устройство: стек, сокеты и граф не меняются.

Пару собирает плата, а не приложение: фоновая работа стека и сам стек
рождаются одним `embassy_net::new` и уходят разным узлам, поэтому `Board::new`
строит её один раз, а граф делит по полям (`board.net.runner` — узлу `NET`,
`board.net.stack` — узлам с сокетами). Затравка стека — {% if rng_irq != "" %}первое слово аппаратного
генератора (`bsp::rng`){% else %}из заводского номера (`seed_from_uid`: RNG у чипа нет){% endif %}.

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
{% if cache contains "scb_d" %}
   // Рядом, статиком модуля: очередь кадров (дескрипторы и буферы, ~12 KiB) —
   // память DMA блока, поэтому в некэшируемом регионе, а не в обычном `static`.
   #[unsafe(link_section = ".dma_nc")]
   static mut QUEUE: MaybeUninit<PacketQueue<4, 4>> = MaybeUninit::uninit();

   // В `new`, вместо `Device::new(mac_from_uid(uid))`:
   // SAFETY: `new` зовётся один раз, из `Board::new`; других ссылок на `QUEUE` нет.
   let queue = unsafe { &mut *(&raw mut QUEUE) };
   PacketQueue::init(queue);
   // SAFETY: `init` обнулил очередь — по документации `PacketQueue::init` это
   // допустимое значение.
   let queue = unsafe { queue.assume_init_mut() };
   Ethernet::new(
       queue, r.eth, Irqs,
       r.ref_clk, r.crs, r.rx_d0, r.rx_d1, r.tx_d0, r.tx_d1, r.tx_en,
       mac_from_uid(uid), r.sma, r.mdio, r.mdc,
   )
   ```

   Очередь — не `ConstStaticCell`: секция `.dma_nc` — `NOLOAD`, и флаг «уже
   взят» внутри ячейки при старте был бы мусором. Обычный `static` здесь
   ломается молча: дескрипторы и кадры, которые пишет DMA, ядро читает из кэша
   данных устаревшими (`docs/clocks.md`, «Кэш данных ядра M7 и DMA»). Оба
   `unsafe` — новые места: впишите их в список в комментарии к `unsafe_code` в
   `crates-cross/Cargo.toml`, каждое — под `#[expect(unsafe_code, reason = …)]`.
{% else %}
   // Рядом, статиком модуля: очередь кадров (дескрипторы и буферы, ~12 KiB) —
   // память блока, а не стека.
   static QUEUE: ConstStaticCell<PacketQueue<4, 4>> = ConstStaticCell::new(PacketQueue::new());

   // В `new`, вместо `Device::new(mac_from_uid(uid))`:
   Ethernet::new(
       QUEUE.take(), r.eth, Irqs,
       r.ref_clk, r.crs, r.rx_d0, r.rx_d1, r.tx_d0, r.tx_d1, r.tx_en,
       mac_from_uid(uid), r.sma, r.mdio, r.mdc,
   )
   ```
{% endif %}
   Адрес PHY на шине MDIO задавать не нужно: `Ethernet::new` строит
   `GenericPhy::new_auto` и при старте перебирает адреса 0–31 (не ответил ни
   один — паника «PHY did not respond»). Нестандартный PHY или фиксированный
   адрес — `Ethernet::new_with_phy` с `GenericPhy::new(Sma::new(r.sma, r.mdio, r.mdc), адрес)`.
   Выравнивание очереди задаёт сам тип (`#[repr(align(8))]` у буферов).

   Точные аргументы зависят от версии блока Ethernet чипа (v1 у F4/F7, v2 у
   H5/H7) — сверяйте с `embassy-stm32` 0.6 (`src/eth/v1`, `src/eth/v2`).
   `new` получает группу `eth` из `resources.rs`: `Board::new` уже зовёт
   `crate::net::new(uid, seed, r.eth)` (`docs/resources.md`); выводы добавляются в группу.

3. В графе — тип устройства во фрагменте `NET`: он уже назван через
   `bsp::net::Device`, менять ничего не нужно.

4. В `target-tests` — не ронять драйвер. `#[init]` там отдаёт тесту `Board`
   целиком, и после теста он дропается, а с ним `board.net` и `Ethernet`. `Drop`
   у него в блоке v2 (H5/H7) ждёт, пока опустеет очередь приёма MTL
   (`embassy-stm32` 0.6, `src/eth/v2/mod.rs`, `impl Drop`), а без узла `NET` кадры
   после подъёма линка никто не забирает, и тест висит до таймаута. Заберите
   сеть без деструктора первой строкой теста — МК сбрасывается перед каждым
   тестом, утечки нет, а остальные поля платы тест по-прежнему берёт по
   значению (`ManuallyDrop<Board>` тут не годится: из-под `Deref` поле не
   переместить):

   ```rust,ignore
   core::mem::forget(board.net);
   ```

## Адрес, MAC и порты

* Адрес — по DHCP (`Config::dhcpv4` в `crates-cross/bsp/src/net.rs`). Статический —
  одной строкой там же:
  `Config::ipv4_static(StaticConfigV4 { address: Ipv4Cidr::new(Ipv4Address::new(192, 168, 1, 50), 24), gateway: None, dns_servers: Default::default() })`.
* MAC — из заводского номера чипа (`mac_from_uid` в `bsp/src/net.rs`), локально
  администрируемый. Свой, выданный производителем, — подставьте там же.
{%- if link_kinds contains "tcp" or link_kinds contains "udp" %}
* Порты протоколов — `PORT` в {% if link_kinds contains "tcp" %}`crates-cross/app/src/link/tcp.rs`{% if link_kinds contains "udp" %} и `udp.rs`{% endif %}{% else %}`crates-cross/app/src/link/udp.rs`{% endif %}: их
  задаёт протокол, а не плата.
{%- endif %}
* Сокетов в стеке — `NET_SOCKETS` в `[env]` `crates-cross/.cargo/config.toml`
  (`bsp::net`): его считает генерация — DHCP-клиент, по одному на сетевой
  транспорт и на каждое соединение служб. Свой сокет — плюс один.
{%- if has_tcp == "true" %}
* TCP: клиент один, `TcpServer` принимает следующего, когда текущий отключился.
{%- endif %}
{%- if has_udp == "true" %}
* UDP: ответ уходит отправителю последней датаграммы.
{%- endif %}
