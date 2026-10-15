#![doc = include_str!("../../../docs/modules/bsp-usb.md")]

use adapters::usb::{CdcStream, Unconnected};
use embassy_usb::class::cdc_acm::{CdcAcmClass, State};
use embassy_usb::{Builder, Config, UsbDevice};
use static_cell::StaticCell;

/// USB-драйвер платы — пока заглушка, до USB-блока чипа (см. модуль).
pub type UsbDriver = Unconnected;

/// Идентификаторы устройства, которые увидит хост. `0xC0DE:0xCAFE` — пара из
/// примеров `embassy-usb`, не зарегистрированная: для своего изделия нужны
/// свои VID/PID.
const VID: u16 = 0xC0DE;
const PID: u16 = 0xCAFE;

/// Пакет CDC — 64 байта: предел full-speed для bulk-точек.
const PACKET: u16 = 64;

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
    static CONFIG_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static BOS_DESCRIPTOR: StaticCell<[u8; 256]> = StaticCell::new();
    static CONTROL: StaticCell<[u8; 64]> = StaticCell::new();
    static CDC_STATE: StaticCell<State<'static>> = StaticCell::new();
    static RX: StaticCell<[u8; PACKET as usize]> = StaticCell::new();
    defmt::warn!("bsp: USB не подключён — хост устройство не увидит, протокол ждёт впустую");

    let mut config = Config::new(VID, PID);
    config.product = Some("{{project-name}}");
    config.serial_number = Some(embassy_stm32::uid::uid_hex());

    let mut builder = Builder::new(
        Unconnected::default(),
        config,
        CONFIG_DESCRIPTOR.init([0; 256]),
        BOS_DESCRIPTOR.init([0; 256]),
        &mut [],
        CONTROL.init([0; 64]),
    );
    let class = CdcAcmClass::new(&mut builder, CDC_STATE.init(State::new()), PACKET);
    let device = builder.build();
    let (tx, rx) = class.split();
    Usb {
        device,
        cdc: CdcStream::new(tx, rx.into_buffered(RX.init([0; PACKET as usize]))),
    }
}
