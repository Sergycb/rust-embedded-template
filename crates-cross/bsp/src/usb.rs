#![doc = include_str!("../../../docs/modules/bsp-usb.md")]

{%- assign xs = links | split: "," %}
{%- assign usb_index = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
{%- assign usb_index = usb_index | plus: 1 %}

/// USB `{{ f[2] }}` платы — транспорт `{{ f[4] }}`: устройство `embassy-usb` с одним
/// классом CDC ACM.
{%- if f[4] == "usb" %}
#[expect(
    clippy::module_inception,
    reason = "имя транспорта — имя блока `USB`, а модуль устройств — `usb`"
)]
{%- endif %}
pub mod {{ f[4] }} {
    use adapters_hw::usb::CdcBuffers;
    use embassy_usb::Config;
    use embassy_usb::class::cdc_acm::State;
    use embassy_usb::driver::Driver;
    use static_cell::{ConstStaticCell, StaticCell};

    /// Шестнадцатеричные цифры серийного номера.
    const HEX: &[u8; 16] = b"0123456789abcdef";

    /// Устройство `embassy-usb` — то, что исполняет узел `USB_{{ f[4] | upcase }}`.
    pub type Device<D> = embassy_usb::UsbDevice<'static, D>;

    /// Поток CDC — то, на чём стоит {% if f[3] != "none" %}транспорт протокола{% else %}ваш узел: в сигнатуре его задачи тип нужен
    /// по имени (задачи embassy не generic){% endif %}.
    pub type Cdc<D> = adapters_hw::usb::CdcStream<'static, D>;

    /// Устройство и поток CDC — из одного `Builder`: классы регистрируются до
    /// того, как построено устройство, поэтому плата собирает их вместе.
    /// Поле `Board` — `device` уезжает в узел `USB_{{ f[4] | upcase }}`, `cdc` — под протокол.
    pub type Usb<D> = adapters_hw::usb::Usb<D>;

    // Память устройства: `Builder` и класс держат её `&'static mut`.
    // `ConstStaticCell`, а не `StaticCell`: значения известны при компиляции и
    // лежат в `.bss`/`.data`, а не собираются на стеке и копируются при старте.
    // Кроме `CDC_STATE`: `ConstStaticCell` требует `Send`, а состояние класса его
    // не реализует (внутри ссылка на `RefCell`).
    static BUFFERS: ConstStaticCell<CdcBuffers> = ConstStaticCell::new(CdcBuffers::new());
    static CDC_STATE: StaticCell<State<'static>> = StaticCell::new();

    // Серийный номер: заводской номер чипа{% if usb_index > 1 %} и номер устройства через дефис — у
    // двух устройств одной платы с общими VID/PID он обязан различаться, иначе
    // хост путает их между подключениями{% endif %}.
    static SERIAL: ConstStaticCell<[u8; 26]> = ConstStaticCell::new([0; 26]);

    /// Собирает устройство с одним классом CDC ACM (`adapters_hw::usb::Usb::new`).
    ///
    /// Серийный номер — заводской номер чипа (`uid`){% if usb_index > 1 %} с номером устройства
    /// `-{{ usb_index }}`{% endif %}. Драйвер чипа приходит готовым: USB-блок чипа — не дело
    /// платы.
    pub(crate) fn new<D: Driver<'static>>(driver: D, uid: [u8; 12]) -> Usb<D> {
        let mut config = Config::new(super::VID, super::PID);
        config.product = Some("{{project-name}}");

        let serial = SERIAL.take();
        for (index, byte) in uid.iter().enumerate() {
            serial[2 * index] = HEX[usize::from(byte >> 4)];
            serial[2 * index + 1] = HEX[usize::from(byte & 0xF)];
        }
{%- if usb_index > 1 %}
        serial[24..].copy_from_slice(b"-{{ usb_index }}");
{%- endif %}
        let serial: &'static [u8; 26] = serial;
{%- if usb_index > 1 %}
        let serial = core::str::from_utf8(serial).expect("заводской номер и суффикс — ASCII");
{%- else %}
        let serial = core::str::from_utf8(&serial[..24]).expect("заводской номер — ASCII");
{%- endif %}
        config.serial_number = Some(serial);

        Usb::new(driver, config, BUFFERS.take(), CDC_STATE.init(State::new()))
    }
}
{%- endif %}
{%- endfor %}

/// Идентификаторы устройства, которые увидит хост. `0xC0DE:0xCAFE` — пара из
/// примеров `embassy-usb`, не зарегистрированная: для своего изделия нужны
/// свои VID/PID.
const VID: u16 = 0xC0DE;
const PID: u16 = 0xCAFE;
