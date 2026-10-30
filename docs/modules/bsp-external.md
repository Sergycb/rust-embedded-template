Внешняя флеш платы — Winbond W25Q (W25Q10..W25Q128, серии JV, JW, FV) на
блоке `{{ext_flash_block}}`: на ней {% if ota == "true" and ota_store == "external" %}`STATE` и `DFU` обновления{% if config_store == "external" %} и раздел настроек{% endif %}{% else %}раздел настроек{% endif %}.

Драйвер — `w25q` из [hw-drivers]({{hw_drivers}}),
асинхронный (`ExternalFlash` здесь){% if ota == "true" and ota_store == "external" %}; у bootloader'а — блокирующий
(`crates-cross/boot/src/external.rs`){% endif %}. Объём он узнаёт по JEDEC ID при
`init`{% if ext_flash_bus != "spi" %}, там же ставит бит QE (четыре линии данных){% endif %}; разделы посчитаны при
генерации, и микросхема меньше их конца (символ `memory.x` `__ext_flash_end`,
`bsp::layout::ext_flash_end`) не годится —
`Board::new` говорит об этом в лог. Аналоги других производителей (GD25Q,
XM25QH…) `init` не проходят: драйвер сверяет производителя.

Шаблон выводы не выбирает (`docs/architecture.md`), поэтому шина под
драйвером — заглушка `bsp::stub::FlashBus`: `init` не проходит, и каждая
операция с флешью отвечает ошибкой. Проект собирается и работает, только {% if ota == "true" and ota_store == "external" %}образ
не принимается, а bootloader загружает текущий, не обменивая разделы{% else %}настройки не сохраняются{% endif %}.

Разделы — одним объектом под одним мьютексом (`ExternalMutex`): микросхема
одна, и операции разных разделов не должны перемежаться посреди команды.

## Как подключить микросхему

Блок шины уже лежит в группе `ext_flash` (`resources.rs`). Выводы и
DMA-каналы к нему — из справочника по чипу:

```text
cargo xtask pins {{ext_flash_block}} --snippet
```

Перенесите в группу `ext_flash` выводы, разведённые на плате, и DMA-каналы, а
`bind_interrupts!` из заготовки — в `bsp/src/external.rs`, дописав в него
прерывания DMA-каналов (`dma::InterruptHandler`).
{%- if ext_flash_bus == "spi" %}

1. Шина — `w25q::spi::SpiAdapter` над `SpiDevice`: SPI-блок и вывод CS
   микросхемы (группа `ext_flash { spi, sck, mosi, miso, cs, tx_dma, rx_dma }`).
   В `bsp/src/external.rs` вместо заглушки:

   ```rust,ignore
   use embassy_embedded_hal::shared_bus::asynch::spi::SpiDevice;
   use embassy_stm32::gpio::{Level, Output, Speed};
   use embassy_stm32::mode::Async;
   use embassy_stm32::spi::mode::Master;
   use embassy_stm32::spi::{Config, Spi};

   type SpiBus = Mutex<NoopRawMutex, Spi<'static, Async, Master>>;
   static SPI: StaticCell<SpiBus> = StaticCell::new();

   pub type Bus = w25q::spi::SpiAdapter<
       SpiDevice<'static, NoopRawMutex, Spi<'static, Async, Master>, Output<'static>>,
   >;
   ```

   и в `new` (параметр — `r: crate::resources::ExtFlashResources`):

   ```rust,ignore
   let spi = Spi::new(r.spi, r.sck, r.mosi, r.miso, r.tx_dma, r.rx_dma, Irqs, Config::default());
   let cs = Output::new(r.cs, Level::High, Speed::VeryHigh);
   let bus = w25q::spi::SpiAdapter::new(SpiDevice::new(SPI.init(Mutex::new(spi)), cs));
   let mut flash = ExternalFlash::new(bus, embassy_time::Delay);
   ```

   W25Q работает в режимах SPI 0 и 3 (`Config::default()` — режим 0); частоту
   поднимайте `Config::frequency` — предел ставит блок SPI, а не микросхема.
{%- if ota == "true" and ota_store == "external" %}

2. В bootloader'е (`crates-cross/boot/src/external.rs`) — тот же блок, но
   блокирующий. Периферию `boot` берёт из `p` в `main` (групп `resources.rs` у
   него нет), в `crates-cross/boot/Cargo.toml` добавьте
   `static_cell = { workspace = true }`:

   ```rust,ignore
   use core::cell::RefCell;
   use embassy_embedded_hal::shared_bus::blocking::spi::SpiDevice;
   use embassy_stm32::gpio::Output;
   use embassy_stm32::mode::Blocking;
   use embassy_stm32::spi::mode::Master;
   use embassy_stm32::spi::Spi;
   use embassy_sync::blocking_mutex::NoopMutex;
   use embassy_sync::blocking_mutex::raw::NoopRawMutex;

   static SPI: static_cell::StaticCell<NoopMutex<RefCell<Spi<'static, Blocking, Master>>>> =
       static_cell::StaticCell::new();

   pub(crate) type Bus = w25q::spi::SpiAdapter<
       SpiDevice<'static, NoopRawMutex, Spi<'static, Blocking, Master>, Output<'static>>,
   >;
   pub(crate) type ExternalFlash = w25q::device::W25qSync<Bus, cortex_m::delay::Delay>;

   pub(crate) fn new(sys_hz: u32, spi: Spi<'static, Blocking, Master>, cs: Output<'static>) -> ExternalFlash {
       let spi = SPI.init(NoopMutex::new(RefCell::new(spi)));
       let bus = w25q::spi::SpiAdapter::new(SpiDevice::new(spi, cs));
       let core = cortex_m::Peripherals::take().expect("boot: периферия ядра уже взята");
       let delay = cortex_m::delay::Delay::new(core.SYST, sys_hz);
       let mut flash = ExternalFlash::new(bus, delay);
       // дальше — `init`, как в шаблоне
   }
   ```

   а в `main` — `Spi::new_blocking(p.SPIx, sck, mosi, miso, Config::default())`
   и `Output::new(cs, Level::High, Speed::VeryHigh)` на тех же выводах, что у
   приложения.
{%- endif %}
{%- else %}

1. Шина — своя обёртка над `embassy_stm32::{% if ext_flash_bus == "quadspi" %}qspi::Qspi{% else %}ospi::Ospi{% endif %}`: драйвер описывает каждую
   команду фазами (`w25q::bus::Command` — инструкция, адрес, dummy, данные и
   число линий у каждой), и обёртка переносит их в `TransferConfig` блока.
   Обёртка, а не `impl` на самом `{% if ext_flash_bus == "quadspi" %}Qspi{% else %}Ospi{% endif %}`: и трейт, и тип чужие для `bsp`.
   Фаза alternate bytes драйверу не нужна — без неё обходятся все его
   команды, кроме memory-mapped режима с continuous read, которым шаблон не
   пользуется.
{%- if ext_flash_bus == "quadspi" %}

   Группа — `ext_flash { qspi, io0, io1, io2, io3, ncs, clk, dma }` (выводы
   первого банка, `BK1_*`). В `bsp/src/external.rs` вместо заглушки:

   ```rust,ignore
   use embassy_stm32::mode::Async;
   use embassy_stm32::peripherals::QUADSPI;
   use embassy_stm32::qspi::enums::{DummyCycles, QspiWidth};
   use embassy_stm32::qspi::{Qspi, TransferConfig};
   use w25q::bus::{Command, Data, Lanes};

   /// QUADSPI платы — шина драйвера W25Q.
   pub struct Bus(Qspi<'static, QUADSPI, Async>);

   // Вручную: у `Qspi` нет `Debug`.
   impl core::fmt::Debug for Bus {
       fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
           f.write_str("Bus")
       }
   }

   /// Команда, которую QUADSPI выразить не может.
   #[derive(Debug, defmt::Format)]
   pub struct Unsupported;

   fn width(lanes: Lanes) -> QspiWidth {
       match lanes {
           Lanes::Quad => QspiWidth::QUAD,
           _ => QspiWidth::SING,
       }
   }

   fn config(command: &Command, data_present: bool) -> Result<TransferConfig, Unsupported> {
       if command.alternate.is_some() {
           return Err(Unsupported);
       }
       Ok(TransferConfig {
           iwidth: command.instruction.map_or(QspiWidth::NONE, |i| width(i.lanes)),
           instruction: command.instruction.map_or(0, |i| i.opcode),
           awidth: command.address.map_or(QspiWidth::NONE, |a| width(a.lanes)),
           address: command.address.map(|a| a.value),
           dwidth: if data_present { width(command.data_lanes) } else { QspiWidth::NONE },
           dummy: match command.dummy_cycles {
               0 => DummyCycles::_0,
               8 => DummyCycles::_8,
               _ => return Err(Unsupported),
           },
       })
   }

   impl w25q::bus::FlashBusAsync for Bus {
       type Error = Unsupported;
       const LANES: Lanes = Lanes::Quad;

       async fn transfer(&mut self, command: &Command, data: Data<'_>) -> Result<(), Unsupported> {
           match data {
               Data::None => self.0.blocking_command(config(command, false)?),
               Data::Read(buf) => self.0.read_dma(buf, config(command, true)?).await,
               Data::Write(buf) => self.0.write_dma(buf, config(command, true)?).await,
           }
           Ok(())
       }
   }
   ```

   и в `new` (параметр — `r: crate::resources::ExtFlashResources`):

   ```rust,ignore
   let qspi = Qspi::new_bank1(
       r.qspi, r.io0, r.io1, r.io2, r.io3, r.clk, r.ncs, r.dma, Irqs,
       embassy_stm32::qspi::Config::default(),
   );
   let mut flash = ExternalFlash::new(Bus(qspi), embassy_time::Delay);
   ```

   Размер адреса — в `Config` блока, а не в команде: у W25Q до 128 Mbit он
   всегда 24 бита (`AddressSize::_24bit`, умолчание); частоту шины задаёт
   `prescaler` (умолчание — 128).
{%- else %}

   Группа — `ext_flash { ospi, clk, ncs, io0, io1, io2, io3 }`. У чипов с
   мультиплексором `OCTOSPIM` (H72x/H73x, L4+, U5) выводы — его, с приставкой
   порта: `cargo xtask pins OCTOSPIM --snippet`, сигналы `P1_*` или `P2_*`
   одного порта. Блок — блокирующий: на H72x/H73x DMA у OCTOSPI — MDMA,
   которого в `embassy-stm32` нет; где DMA есть, `Ospi::new_quadspi` и
   асинхронные `read`/`write` подставляются в той же обёртке. В
   `bsp/src/external.rs` вместо заглушки:

   ```rust,ignore
   use embassy_stm32::mode::Blocking;
   use embassy_stm32::ospi::{AddressSize, DummyCycles, Ospi, OspiError, OspiWidth, TransferConfig};
   use embassy_stm32::peripherals::{{ext_flash_block}};
   use w25q::bus::{Command, Data, Lanes};

   /// OCTOSPI платы — шина драйвера W25Q.
   pub struct Bus(Ospi<'static, {{ext_flash_block}}, Blocking>);

   // Вручную: у `Ospi` нет `Debug`.
   impl core::fmt::Debug for Bus {
       fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
           f.write_str("Bus")
       }
   }

   fn width(lanes: Lanes) -> OspiWidth {
       match lanes {
           Lanes::Quad => OspiWidth::QUAD,
           _ => OspiWidth::SING,
       }
   }

   fn config(command: &Command, data_present: bool) -> Result<TransferConfig, OspiError> {
       if command.alternate.is_some() {
           return Err(OspiError::InvalidCommand);
       }
       Ok(TransferConfig {
           iwidth: command.instruction.map_or(OspiWidth::NONE, |i| width(i.lanes)),
           instruction: command.instruction.map(|i| u32::from(i.opcode)),
           adwidth: command.address.map_or(OspiWidth::NONE, |a| width(a.lanes)),
           address: command.address.map(|a| a.value),
           adsize: AddressSize::_24bit,
           dwidth: if data_present { width(command.data_lanes) } else { OspiWidth::NONE },
           dummy: match command.dummy_cycles {
               0 => DummyCycles::_0,
               8 => DummyCycles::_8,
               _ => return Err(OspiError::InvalidCommand),
           },
           ..TransferConfig::default()
       })
   }

   impl w25q::bus::FlashBusAsync for Bus {
       type Error = OspiError;
       const LANES: Lanes = Lanes::Quad;

       fn transfer(
           &mut self,
           command: &Command,
           data: Data<'_>,
       ) -> impl Future<Output = Result<(), OspiError>> {
           core::future::ready(match data {
               Data::None => config(command, false).and_then(|c| self.0.blocking_command(&c)),
               Data::Read(buf) => config(command, true).and_then(|c| self.0.blocking_read(buf, c)),
               Data::Write(buf) => config(command, true).and_then(|c| self.0.blocking_write(buf, c)),
           })
       }
   }
   ```

   и в `new` (параметр — `r: crate::resources::ExtFlashResources`):

   ```rust,ignore
   let ospi = Ospi::new_blocking_quadspi(
       r.ospi, r.clk, r.io0, r.io1, r.io2, r.io3, r.ncs,
       embassy_stm32::ospi::Config::default(),
   );
   let mut flash = ExternalFlash::new(Bus(ospi), embassy_time::Delay);
   ```

   Частоту шины задаёт `clock_prescaler` в `Config`, объём —
   `device_size`.
{%- endif %}
{%- if ota == "true" and ota_store == "external" %}

2. В bootloader'е (`crates-cross/boot/src/external.rs`) — та же обёртка над
   блокирующим блоком (`{% if ext_flash_bus == "quadspi" %}Qspi::new_blocking_bank1{% else %}Ospi::new_blocking_quadspi{% endif %}`) с
   `impl w25q::bus::FlashBusSync`: тот же `config`, а `transfer` зовёт
   `blocking_command`/`blocking_read`/`blocking_write` прямо, без future.
   Периферию `boot` берёт из `p` в `main` (групп `resources.rs` у него нет).
{%- endif %}
{%- endif %}

После этого `bsp::external::connected()` возвращает `true`, и target-тесты
перестают пропускать проверки внешней флеши.
