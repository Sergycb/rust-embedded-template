//! Вход узла `MQTT`, который compose-site собирает руками.

/// Вход узла `MQTT`: стек, сведения, префикс тем, откуда адрес брокера и
/// память соединения `D` с ним.
///
/// Соединение собирает узел (`D::assemble(stack, link)` — порт
/// [`Assemble`](ports::Assemble)) на том же стеке; compose-site раздаёт только
/// стек платы и ссылки на свои статики. Написан руками, а не выведен
/// фрагментом: слот соединения инициализируется сборкой, а не чтением поля.
pub struct MqttWiring<B, D: ports::Assemble<Source = crate::net::Stack<'static>>> {
    /// Сетевой стек — ради адреса платы и MAC в `info` и под соединение.
    pub stack: crate::net::Stack<'static>,
    /// Сведения для `info`.
    pub info: crate::device::DeviceInfo,
    /// Префикс тем, не длиннее `PREFIX_MAX`.
    pub prefix: &'static str,
    /// Откуда адрес брокера.
    pub broker: B,
    /// Память соединения с брокером.
    pub link: D::Buffers,
    // {%- if ota == "true" %}
    /// Клиент входа `CMD` узла OTA — для `cmd/reboot`.
    pub ota: crate::ota::OtaClient,
    // {%- endif %}
}

// Вручную: у стека `embassy-net` нет `Debug`.
impl<B, D: ports::Assemble<Source = crate::net::Stack<'static>>> core::fmt::Debug
    for MqttWiring<B, D>
{
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("MqttWiring")
            .field("prefix", &self.prefix)
            .finish_non_exhaustive()
    }
}
