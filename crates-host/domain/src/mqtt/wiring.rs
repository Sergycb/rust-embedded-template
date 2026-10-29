//! Вход узла `MQTT`.

/// Вход узла `MQTT`: стек, сведения, префикс тем, откуда адрес брокера и
/// соединение `D` с ним — на том же стеке, его строит compose-site.
pub struct MqttWiring<B, D> {
    /// Сетевой стек — ради адреса платы и MAC в `info` и под соединение.
    pub stack: crate::net::Stack<'static>,
    /// Сведения для `info`.
    pub info: crate::device::DeviceInfo,
    /// Префикс тем, не длиннее `PREFIX_MAX`.
    pub prefix: &'static str,
    /// Откуда адрес брокера.
    pub broker: B,
    /// Соединение с брокером.
    pub link: D,
    // {%- if ota == "true" %}
    /// Клиент входа `CMD` узла OTA — для `cmd/reboot`.
    pub ota: crate::ota::OtaClient,
    // {%- endif %}
}

// Вручную: у стека `embassy-net` нет `Debug`.
impl<B, D> core::fmt::Debug for MqttWiring<B, D> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MqttWiring")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}
