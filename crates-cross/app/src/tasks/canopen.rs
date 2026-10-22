//! Узлы `CANOPEN_*`: узел CANopen, адрес 1, по модулю на шину CAN.
{%- assign xs = links | split: "," %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, как в графе (`crate::graph`). {% endcomment %}
{%- assign client = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "canopen" %}

/// Узел `CANOPEN_{{ f[4] | upcase }}`.
pub mod {{ f[4] }} {
    use embassy_executor::Spawner;

    /// Хранилище узла: {% if config == "true" %}раздел настроек платы{% else %}раздела настроек нет — объекты и адрес не сохраняются{% endif %}.
    type Store = {% if config == "true" %}crate::settings::Settings{% else %}adapters::settings::Unavailable{% endif %};

    /// Шина узла.
    type Bus = bsp::can::{{ f[4] }}::Can;

    /// Из чего собрать хранилище: {% if config == "true" %}раздел настроек платы{% else %}не из чего{% endif %}.
    type StoreSource = {% if config == "true" %}bsp::settings::ConfigFlash{% else %}(){% endif %};

    #[embassy_executor::task]
    async fn task(mut bus: Bus, store: StoreSource) {
        // Хранилище — над рычагом платы, как в слоте фрагмента `CANOPEN_FRAG`.
        let mut store = <Store as domain::Assemble>::assemble(store, ());
        let mut node_id = 1;
        let mut dictionary = domain::canopen::dictionary();
        let mut persist = {{config}};
        domain::canopen::task(domain::canopen::CanOpenCtx {
            bus: &mut bus,
            node_id: &mut node_id,
            dictionary: &mut dictionary,
            settings: &mut store,
            persist: &mut persist,
{%- if ota == "true" %}
            ota_cmd: &mut domain::ota::OtaClient::new(
                crate::tasks::ota::CMD.requester(),
                domain::ota::ClientId({{ client }}),
            ),
{%- endif %}
            index: 0,
        })
        .await;
    }

    /// Спавнит узел.
    pub fn spawn(spawner: Spawner, bus: Bus, store: StoreSource) {
        let token = task(bus, store);
        spawner.spawn(token.expect("задача CANOPEN_{{ f[4] | upcase }} свежая: спавнится один раз"));
    }
}
{%- endif %}
{%- if f[3] != "none" %}{% assign client = client | plus: 1 %}{% endif %}
{%- endfor %}
