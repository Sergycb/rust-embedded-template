//! Периферия, которой ещё нет на плате, — заглушки в слотах драйверов.
//!
//! Шаблон не выбирает выводы за проект; как ведёт себя заглушка и как её
//! заменить — `docs/architecture.md`, «`Board` отдаёт рычаги платы, а не
//! периферию и не протоколы».
{%- if has_usb == "true" %}

/// USB-блок, которого ещё нет на плате, — место под драйвер `embassy-stm32`.
///
/// Устройство `embassy-usb` и класс CDC над ним настоящие, а шина никогда не
/// просыпается — хост устройство не видит. Реализация — `adapters_hw::usb::stub`:
/// на ней же host-тесты гоняют узел `USB`.
pub use adapters_hw::usb::stub::{NoBus, NoEndpoint, UsbDriver};
{%- endif %}
{%- if has_net == "true" %}

/// Сетевое устройство, которого ещё нет на плате, — место под Ethernet.
///
/// Стек `embassy-net` над ним настоящий, а линк всегда опущен: кадров нет ни
/// на приёме, ни на передаче. Реализация — `adapters_hw::net::stub`: на ней же
/// host-тесты гоняют узел `NET`.
pub use adapters_hw::net::stub::{Device, NoFrame};
{%- endif %}
{%- if has_uart == "true" %}

/// UART, который ещё не подключили, — место под `BufferedUart` платы.
#[derive(Debug, Default)]
pub struct Serial;

impl embedded_io_async::ErrorType for Serial {
    type Error = core::convert::Infallible;
}

impl embedded_io_async::Read for Serial {
    async fn read(&mut self, _: &mut [u8]) -> Result<usize, Self::Error> {
        core::future::pending().await
    }
}

impl embedded_io_async::ReadReady for Serial {
    fn read_ready(&mut self) -> Result<bool, Self::Error> {
        Ok(false)
    }
}

impl embedded_io_async::Write for Serial {
    fn write(&mut self, buf: &[u8]) -> impl Future<Output = Result<usize, Self::Error>> {
        core::future::ready(Ok(buf.len()))
    }

    fn flush(&mut self) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Ok(()))
    }
}
{%- endif %}
{%- if has_can == "true" %}

/// CAN, который ещё не подключили, — место под контроллер платы. Кадр — тот
/// же тип, что у драйвера `embassy-stm32`, но сам драйвер `ports::CanBus` не
/// реализует: замена — свой тип над контроллером, реализующий порт, и псевдоним
/// на него (`docs/modules/bsp-can.md`).
#[derive(Debug, Default)]
pub struct Can;

impl ports::CanBus for Can {
    type Frame = embassy_stm32::can::Frame;
    type Error = core::convert::Infallible;

    async fn recv(&mut self) -> Result<Self::Frame, ports::LinkError<Self::Error>> {
        core::future::pending().await
    }

    fn send(
        &mut self,
        _: &Self::Frame,
    ) -> impl Future<Output = Result<(), ports::LinkError<Self::Error>>> {
        core::future::ready(Ok(()))
    }
}
{%- endif %}
{%- if external_flash == "true" %}

/// Шина внешней флеши, к которой ещё не подключили микросхему, — место под
/// {% if ext_flash_bus == "spi" %}SPI{% elsif ext_flash_bus == "quadspi" %}QUADSPI{% else %}OCTOSPI{% endif %} платы (`bsp::external::Bus`).
///
/// Драйвер W25Q над ней настоящий, а каждая команда отвечает
/// [`NotConnected`]: `init` не проходит, и драйвер отказывает в любой
/// операции — {% if config_store == "external" %}настройки не сохраняются{% endif %}{% if config_store == "external" and ota_store == "external" and ota == "true" %}, {% endif %}{% if ota == "true" and ota_store == "external" %}образ не принимается{% endif %}, но ничего не
/// падает.{% if ota == "true" and ota_store == "external" %} Асинхронная — для приложения, блокирующая — для bootloader'а: тот
/// с ней не обменивает разделы, а загружает текущий образ.{% endif %}
#[derive(Debug, Default)]
pub struct Bus;

/// Ответ [`Bus`] на любую команду.
#[derive(Debug, Clone, Copy, PartialEq, Eq, defmt::Format)]
pub struct NotConnected;

impl w25q::bus::FlashBusAsync for Bus {
    type Error = NotConnected;

    /// Ширина настоящей шины: по ней драйвер выбирает команды{% if ext_flash_bus != "spi" %} (и ставит
    /// бит QE){% endif %}.
    const LANES: w25q::bus::Lanes = w25q::bus::Lanes::{% if ext_flash_bus == "spi" %}Single{% else %}Quad{% endif %};

    fn transfer(
        &mut self,
        _: &w25q::bus::Command,
        _: w25q::bus::Data<'_>,
    ) -> impl Future<Output = Result<(), Self::Error>> {
        core::future::ready(Err(NotConnected))
    }
}

impl w25q::bus::FlashBusSync for Bus {
    type Error = NotConnected;

    const LANES: w25q::bus::Lanes = <Self as w25q::bus::FlashBusAsync>::LANES;

    fn transfer(
        &mut self,
        _: &w25q::bus::Command,
        _: w25q::bus::Data<'_>,
    ) -> Result<(), Self::Error> {
        Err(NotConnected)
    }
}
{%- endif %}
