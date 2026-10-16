#![doc = include_str!("../../../docs/modules/bsp-usb.md")]

use adapters::usb::{CdcStream, Unconnected};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::{Builder, Config, UsbDevice};
use static_cell::{ConstStaticCell, StaticCell};

/// USB-драйвер платы — пока заглушка, до USB-блока чипа (см. модуль).
pub type UsbDriver = Unconnected;

/// Идентификаторы устройства, которые увидит хост. `0xC0DE:0xCAFE` — пара из
/// примеров `embassy-usb`, не зарегистрированная: для своего изделия нужны
/// свои VID/PID.
const VID: u16 = 0xC0DE;
const PID: u16 = 0xCAFE;

/// Пакет CDC — 64 байта: предел full-speed для bulk-точек.
const PACKET: u16 = 64;

// Буферы и состояние устройства: `Builder` и класс держат их `&'static mut`.
// `ConstStaticCell`, а не `StaticCell`: значения известны при компиляции и
// лежат в `.bss`/`.data`, а не собираются на стеке и копируются при старте.
// Кроме `CDC_STATE`: `ConstStaticCell` требует `Send`, а состояние класса его
// не реализует (внутри ссылка на `RefCell`).
static CONFIG_DESCRIPTOR: ConstStaticCell<[u8; 256]> = ConstStaticCell::new([0; 256]);
static BOS_DESCRIPTOR: ConstStaticCell<[u8; 256]> = ConstStaticCell::new([0; 256]);
static CONTROL: ConstStaticCell<[u8; 64]> = ConstStaticCell::new([0; 64]);
static CDC_STATE: StaticCell<State<'static>> = StaticCell::new();
static RX: ConstStaticCell<[u8; PACKET as usize]> = ConstStaticCell::new([0; PACKET as usize]);

/// USB платы: устройство для узла `USB` и поток CDC под протоколом.
pub struct Usb {
    /// USB-устройство — уезжает в узел `USB` графа.
    pub device: UsbDevice<'static, UsbDriver>,
    /// Поток CDC ACM («виртуальный COM-порт») — под транспортом протокола.
    pub cdc: CdcStream<'static, UsbDriver>,
}

// Вручную: у `UsbDevice` нет `Debug`.
impl core::fmt::Debug for Usb {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("Usb").finish_non_exhaustive()
    }
}

/// Собирает USB-устройство с одним классом CDC ACM над драйвером платы.
/// Серийный номер устройства — заводской номер чипа.
pub(crate) fn new() -> Usb {
    defmt::warn!("bsp: USB не подключён — хост устройство не увидит, протокол ждёт впустую");

    let mut config = Config::new(VID, PID);
    config.product = Some("{{project-name}}");
    config.serial_number = Some(embassy_stm32::uid::uid_hex());

    let mut builder = Builder::new(
        Unconnected::default(),
        config,
        CONFIG_DESCRIPTOR.take(),
        BOS_DESCRIPTOR.take(),
        &mut [],
        CONTROL.take(),
    );
    let class = CdcAcmClass::new(&mut builder, CDC_STATE.init(State::new()), PACKET);
    let device = builder.build();
    let (tx, rx) = class.split();
    Usb {
        device,
        cdc: CdcStream::new(tx, rx.into_buffered(RX.take())),
    }
}
