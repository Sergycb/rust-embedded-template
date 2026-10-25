//! Узлы `USB_*`: фоновая работа устройства `embassy-usb`, по модулю на
//! транспорт.
{%- assign xs = links | split: "," %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[0] == "usb" %}

/// Узел `USB_{{ f[4] | upcase }}`.
{%- if f[4] == "usb" %}
#[expect(
    clippy::module_inception,
    reason = "имя транспорта — имя блока `USB`, а модуль узлов — `usb`"
)]
{%- endif %}
pub mod {{ f[4] }} {
    use embassy_executor::Spawner;

    #[embassy_executor::task]
    async fn task(mut device: bsp::usb::{{ f[4] }}::Device) {
{%- if ota == "true" %}
        // На шину — после подтверждения образа (`crate::tasks::ota`).
        crate::tasks::ota::READY_{{ f[4] | upcase }}.wait().await;
{%- endif %}
        domain::usb::task(domain::usb::UsbCtx {
            device: &mut device,
            index: 0,
        })
        .await;
    }

    /// Спавнит узел.
    pub fn spawn(spawner: Spawner, device: bsp::usb::{{ f[4] }}::Device) {
        let token = task(device);
        spawner.spawn(token.expect("задача USB_{{ f[4] | upcase }} свежая: спавнится один раз"));
    }
}
{%- endif %}
{%- endfor %}
