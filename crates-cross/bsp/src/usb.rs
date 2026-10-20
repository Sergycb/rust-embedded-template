#![doc = include_str!("../../../docs/modules/bsp-usb.md")]

/// Идентификаторы устройства, которые увидит хост. `0xC0DE:0xCAFE` — пара из
/// примеров `embassy-usb`, не зарегистрированная: для своего изделия нужны
/// свои VID/PID.
const VID: u16 = 0xC0DE;
const PID: u16 = 0xCAFE;
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}
{%- assign parts = f[4] | split: "_" %}

/// USB `{{ f[2] }}` платы — транспорт `{{ f[4] }}`. Типы — свои у каждого
/// блока: у настоящего драйвера тип зависит от блока.
{%- if f[4] == "usb" %}
#[expect(
    clippy::module_inception,
    reason = "имя транспорта — имя блока `USB`, а модуль драйвера — `usb`"
)]
{%- endif %}
pub mod {{ f[4] }} {
    use adapters::usb::{CdcBuffers, Unconnected};
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

    /// USB-драйвер `{{ f[2] }}` — пока заглушка, до USB-блока чипа (см. модуль `bsp::usb`).
    pub type UsbDriver = Unconnected;

    /// USB `{{ f[2] }}`: устройство для узла `USB_{{ f[4] | upcase }}` и поток CDC под протоколом.
    pub type Usb = adapters::usb::Usb<UsbDriver>;

    /// Поток CDC — то, на чём стоит транспорт протокола.
    pub type Cdc = adapters::usb::CdcStream<'static, UsbDriver>;

    /// Устройство `embassy-usb` — то, что исполняет узел `USB_{{ f[4] | upcase }}`. Псевдоним,
    /// чтобы `app` не называл `embassy-usb`.
    pub type Device = embassy_usb::UsbDevice<'static, UsbDriver>;

    /// Собирает USB-устройство с одним классом CDC ACM над драйвером платы
    /// (`adapters::usb::cdc`). Серийный номер устройства — заводской номер чипа.
    /// Блок приходит группой `resources.rs` — заглушка его не трогает, драйвер
    /// платы соберётся из него и выводов.
    pub(crate) fn new(_resources: crate::resources::{% for part in parts %}{{ part | capitalize }}{% endfor %}Resources) -> Usb {
        defmt::warn!("bsp: {{ f[2] }} не подключён — хост устройства не увидит");

        let mut config = Config::new(super::VID, super::PID);
        config.product = Some("{{project-name}}");
        config.serial_number = Some(embassy_stm32::uid::uid_hex());

        adapters::usb::cdc(
            Unconnected::default(),
            config,
            BUFFERS.take(),
            CDC_STATE.init(State::new()),
        )
    }
}
{%- endif %}
{%- endfor %}
