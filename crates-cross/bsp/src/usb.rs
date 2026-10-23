#![doc = include_str!("../../../docs/modules/bsp-usb.md")]

{%- assign xs = links | split: "," %}
{%- assign usb_index = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
{%- assign usb_index = usb_index | plus: 1 %}
{%- assign parts = f[4] | split: "_" %}

/// USB `{{ f[2] }}` платы — транспорт `{{ f[4] }}`: устройство `embassy-usb` с одним
/// классом CDC ACM.
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

    /// USB-драйвер `{{ f[2] }}` — пока заглушка, до USB-блока чипа (см. модуль `bsp::usb`).
    type Driver = crate::stub::UsbDriver;

    /// Группа блока в `resources.rs`. Псевдоним — ради сигнатуры `new` одной
    /// длины при любом имени блока: rustfmt не переносит её ни при каком.
    type Resources = crate::resources::{% for part in parts %}{{ part | capitalize }}{% endfor %}Resources;

    /// Устройство `embassy-usb` — то, что исполняет узел `USB_{{ f[4] | upcase }}`.
    pub type Device = embassy_usb::UsbDevice<'static, Driver>;

    /// Поток CDC — то, на чём стоит {% if f[3] != "none" %}транспорт протокола{% else %}ваш узел: в сигнатуре его задачи тип нужен
    /// по имени (задачи embassy не generic){% endif %}.
    pub type Cdc = adapters::usb::CdcStream<'static, Driver>;

    /// Устройство и поток CDC — из одного `Builder`: классы регистрируются до
    /// того, как построено устройство, поэтому плата собирает их вместе.
    /// Поле `Board` — `device` уезжает в узел `USB_{{ f[4] | upcase }}`, `cdc` — под протокол.
    pub type Usb = adapters::usb::Usb<Driver>;

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

    /// Собирает устройство с одним классом CDC ACM (`adapters::usb::cdc`).
    /// Серийный номер — заводской номер чипа{% if usb_index > 1 %} с номером устройства
    /// `-{{ usb_index }}`{% endif %}. Блок приходит группой `resources.rs` — заглушка драйвера его
    /// не трогает, драйвер платы соберётся из него и выводов.
    pub(crate) fn new(_resources: Resources) -> Usb {
        defmt::warn!("bsp: {{ f[2] }} не подключён — хост устройства не увидит");
        let mut config = Config::new(super::VID, super::PID);
        config.product = Some("{{project-name}}");
        let uid_hex = embassy_stm32::uid::uid_hex();
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

        adapters::usb::cdc(
            Driver::default(),
            config,
            BUFFERS.take(),
            CDC_STATE.init(State::new()),
        )
    }
}
{%- endif %}
{%- endfor %}

/// Идентификаторы устройства, которые увидит хост. `0xC0DE:0xCAFE` — пара из
/// примеров `embassy-usb`, не зарегистрированная: для своего изделия нужны
/// свои VID/PID.
const VID: u16 = 0xC0DE;
const PID: u16 = 0xCAFE;
