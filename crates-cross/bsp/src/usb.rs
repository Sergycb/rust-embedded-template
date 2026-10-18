#![doc = include_str!("../../../docs/modules/bsp-usb.md")]

use adapters::usb::{CdcBuffers, Unconnected};
use embassy_usb::Config;
use embassy_usb::class::cdc_acm::State;
use static_cell::{ConstStaticCell, StaticCell};

/// Идентификаторы устройства, которые увидит хост. `0xC0DE:0xCAFE` — пара из
/// примеров `embassy-usb`, не зарегистрированная: для своего изделия нужны
/// свои VID/PID.
const VID: u16 = 0xC0DE;
const PID: u16 = 0xCAFE;

// Память устройства: `Builder` и класс держат её `&'static mut`.
// `ConstStaticCell`, а не `StaticCell`: значения известны при компиляции и
// лежат в `.bss`/`.data`, а не собираются на стеке и копируются при старте.
// Кроме `CDC_STATE`: `ConstStaticCell` требует `Send`, а состояние класса его
// не реализует (внутри ссылка на `RefCell`).
static BUFFERS: ConstStaticCell<CdcBuffers> = ConstStaticCell::new(CdcBuffers::new());
static CDC_STATE: StaticCell<State<'static>> = StaticCell::new();

/// USB-драйвер платы — пока заглушка, до USB-блока чипа (см. модуль).
pub type UsbDriver = Unconnected;

/// USB платы: устройство для узла `USB` и поток CDC под протоколом.
pub type Usb = adapters::usb::Usb<UsbDriver>;

/// Собирает USB-устройство с одним классом CDC ACM над драйвером платы
/// (`adapters::usb::cdc`). Серийный номер устройства — заводской номер чипа.
/// Блок приходит группой `resources.rs` — заглушка его не трогает, драйвер
/// платы соберётся из него и выводов.
pub(crate) fn new(_resources: crate::resources::UsbResources) -> Usb {
    defmt::warn!("bsp: USB не подключён — хост устройство не увидит, протокол ждёт впустую");

    let mut config = Config::new(VID, PID);
    config.product = Some("{{project-name}}");
    config.serial_number = Some(embassy_stm32::uid::uid_hex());

    adapters::usb::cdc(
        Unconnected::default(),
        config,
        BUFFERS.take(),
        CDC_STATE.init(State::new()),
    )
}
