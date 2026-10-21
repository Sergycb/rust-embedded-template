//! USB-устройства приложения: по модулю на транспорт, устройство `embassy-usb`
//! с одним классом CDC ACM над драйвером платы (`bsp::Drivers`).
//!
//! Идентификаторы, классы и их память — решение приложения, а не платы:
//! `bsp` отдаёт только драйвер USB-блока. Устройство уезжает в узел `USB_*`,
//! поток CDC — под транспорт протокола (`crate::link`) или в ваш узел.

/// Идентификаторы устройства, которые увидит хост. `0xC0DE:0xCAFE` — пара из
/// примеров `embassy-usb`, не зарегистрированная: для своего изделия нужны
/// свои VID/PID.
const VID: u16 = 0xC0DE;
const PID: u16 = 0xCAFE;
{%- assign xs = links | split: "," %}
{%- assign usb_index = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
{%- assign usb_index = usb_index | plus: 1 %}

/// USB `{{ f[2] }}` — транспорт `{{ f[4] }}`.
{%- if f[4] == "usb" %}
#[expect(
    clippy::module_inception,
    reason = "имя транспорта — имя блока `USB`, а модуль устройств — `usb`"
)]
{%- endif %}
pub mod {{ f[4] }} {
    use adapters::usb::CdcBuffers;
    use embassy_usb::Config;
    use embassy_usb::class::cdc_acm::State;
    use static_cell::{ConstStaticCell, StaticCell};

    // Память устройства: `Builder` и класс держат её `&'static mut`.
    // `ConstStaticCell`, а не `StaticCell`: значения известны при компиляции и
    // лежат в `.bss`/`.data`, а не собираются на стеке и копируются при старте.
    // Кроме `CDC_STATE`: `ConstStaticCell` требует `Send`, а состояние класса его
    // не реализует (внутри ссылка на `RefCell`).
    static BUFFERS: ConstStaticCell<CdcBuffers> = ConstStaticCell::new(CdcBuffers::new());
    static CDC_STATE: StaticCell<State<'static>> = StaticCell::new();
{%- if usb_index > 1 %}

    // Серийный номер: заводской номер чипа и номер устройства через дефис — у
    // двух устройств одной платы с общими VID/PID он обязан различаться, иначе
    // хост путает их между подключениями.
    static SERIAL: ConstStaticCell<[u8; 26]> = ConstStaticCell::new([0; 26]);
{%- endif %}

    /// Драйвер блока — от платы.
    type Driver = bsp::usb::{{ f[4] }}::UsbDriver;

    /// Устройство и поток CDC — из одного `Builder`.
    pub type Usb = adapters::usb::Usb<Driver>;

    /// Устройство `embassy-usb` — то, что исполняет узел `USB_{{ f[4] | upcase }}`.
    pub type Device = embassy_usb::UsbDevice<'static, Driver>;

    /// Поток CDC — то, на чём стоит {% if f[3] != "none" %}транспорт протокола{% else %}ваш узел: в сигнатуре его задачи тип нужен
    /// по имени (задачи embassy не generic){% endif %}.
{%- if f[3] == "none" %}
    #[expect(dead_code, reason = "поток ждёт ваш узел — псевдоним для его задачи")]
{%- endif %}
    pub type Cdc = adapters::usb::CdcStream<'static, Driver>;

    /// Собирает устройство с одним классом CDC ACM над драйвером платы
    /// (`adapters::usb::cdc`). Серийный номер — заводской номер чипа `uid_hex`{% if usb_index > 1 %}
    /// с номером устройства `-{{ usb_index }}`{% endif %}.
    pub fn new(driver: Driver, uid_hex: &'static str) -> Usb {
        let mut config = Config::new(super::VID, super::PID);
        config.product = Some("{{project-name}}");
{%- if usb_index > 1 %}
        let serial = SERIAL.take();
        let (uid, suffix) = serial.split_at_mut(uid_hex.len());
        uid.copy_from_slice(uid_hex.as_bytes());
        suffix.copy_from_slice(b"-{{ usb_index }}");
        let serial = core::str::from_utf8(serial).expect("заводской номер и суффикс — ASCII");
        config.serial_number = Some(serial);
{%- else %}
        config.serial_number = Some(uid_hex);
{%- endif %}

        adapters::usb::cdc(driver, config, BUFFERS.take(), CDC_STATE.init(State::new()))
    }
}
{%- endif %}
{%- endfor %}
