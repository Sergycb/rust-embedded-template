#![doc = include_str!("../../../docs/modules/cpu-can.md")]
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "can" %}
{%- assign parts = f[4] | split: "_" %}

/// Шина CAN `{{ f[2] }}` чипа — транспорт `{{ f[4] }}`.
{%- if f[4] == "can" %}
#[expect(
    clippy::module_inception,
    reason = "имя транспорта — имя блока `CAN`, а модуль драйвера — `can`"
)]
{%- endif %}
pub mod {{ f[4] }} {
    /// Шина `{{ f[2] }}`, которую ещё не подключили, — заглушка до контроллера
    /// чипа. Кадр — тот же тип, что у драйвера `embassy-stm32`, но сам драйвер
    /// `ports::CanBus` не реализует: замена — свой тип над контроллером,
    /// реализующий порт, и псевдоним на него (`docs/modules/cpu-can.md`).
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

    /// Собирает шину `{{ f[2] }}`. Блок приходит группой `wiring.rs` — заглушка
    /// его не трогает, контроллер чипа соберётся из него и выводов.
    pub(crate) fn new(_resources: crate::wiring::{% for part in parts %}{{ part | capitalize }}{% endfor %}Resources) -> Can {
        defmt::warn!("cpu: {{ f[2] }} не подключён — {% if f[3] == "canopen" %}узел CANopen ждёт впустую{% else %}шина ждёт впустую{% endif %}");
        Can
    }
}
{%- endif %}
{%- endfor %}
