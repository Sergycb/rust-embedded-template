//! Узлы `CANOPEN_*`: узел CANopen, адрес 1, по модулю на шину CAN.
{%- assign xs = links | split: "," %}
{%- comment %} Номер клиента OTA — порядковый номер транспорта с протоколом в `links`, как в графе (`crate::graph`). {% endcomment %}
{%- assign client = 0 %}
{%- comment %} Номер ребра входа `ASK` узла OTA — порядковый среди транспортов CANopen, как в `crate::tasks::ota::ASK_EDGES`. {% endcomment %}
{%- assign asker = 0 %}
{%- for l in xs %}
{%- assign f = l | split: ":" %}
{%- if f[3] == "canopen" %}

/// Узел `CANOPEN_{{ f[4] | upcase }}`.
pub mod {{ f[4] }} {
    use embassy_executor::Spawner;
{%- if ota == "true" %}
    use embassy_sync::blocking_mutex::raw::CriticalSectionRawMutex;
    use sync_request::ask::{Asker, TagStore};
{%- endif %}

    /// Хранилище узла: {% if config == "true" %}раздел настроек платы{% else %}раздела настроек нет — объекты и адрес не сохраняются{% endif %}.
    type Store = {% if config == "true" %}adapters::settings::Settings<bsp::ConfigFlash>{% else %}adapters::settings::Unavailable{% endif %};

    /// Шина узла.
    type Bus = bsp::can::{{ f[4] }}::Can;

{%- if ota == "true" %}

    /// Метки команд в полёте на ребре `ASK` узла OTA — одна, по квоте.
    static OTA_TAGS: TagStore<CriticalSectionRawMutex, (), 1> = TagStore::new();
{%- endif %}

    #[embassy_executor::task]
    async fn task(mut bus: Bus, mut store: Store, mut serial: u32) {
        let mut node_id = 1;
        let mut dictionary = domain::canopen::dictionary();
        let mut persist = {{config}};
        domain::canopen::task(domain::canopen::CanOpenCtx {
            bus: &mut bus,
            node_id: &mut node_id,
            dictionary: &mut dictionary,
            settings: &mut store,
            persist: &mut persist,
            serial: &mut serial,
{%- if ota == "true" %}
            ota_client: &mut domain::ota::ClientId({{ client }}),
            // Ребро этой шины к входу `ASK` узла OTA (`crate::tasks::ota`).
            ota_ask: Asker::new(
                core::slice::from_ref(&crate::tasks::ota::ASK_EDGES[{{ asker }}]),
                &OTA_TAGS,
            ),
{%- endif %}
            index: 0,
        })
        .await;
    }

    /// Спавнит узел; `serial` — серийный номер идентичности (0x1018:04).
    pub fn spawn(spawner: Spawner, bus: Bus, store: Store, serial: u32) {
        let token = task(bus, store, serial);
        spawner.spawn(token.expect("задача CANOPEN_{{ f[4] | upcase }} свежая: спавнится один раз"));
    }
}
{%- endif %}
{%- if f[3] == "canopen" %}{% assign asker = asker | plus: 1 %}{% endif %}
{%- if f[3] != "none" %}{% assign client = client | plus: 1 %}{% endif %}
{%- endfor %}
