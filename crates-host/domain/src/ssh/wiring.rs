//! Вход узла `SSH`: что раздаёт ему compose-site.

/// Вход узла `SSH`: стек, сведения, вход пользователя и ключ хоста `K`
/// (`Fixed` — вшитый ключ, `Stored` — над хранилищем настроек).
pub struct SshWiring<K> {
    /// Сетевой стек, на котором узел открывает свой сокет.
    pub stack: crate::net::Stack<'static>,
    /// Сведения для `info`.
    pub info: crate::device::DeviceInfo,
    /// Как пускать пользователя.
    pub login: crate::ssh::Login,
    /// Ключ хоста.
    pub host_key: K,
    // {%- if ota == "true" %}
    /// Клиент входа `CMD` узла OTA — для `reboot`.
    pub ota: crate::ota::OtaClient,
    // {%- endif %}
}

// Вручную: у стека `embassy-net` нет `Debug`.
impl<K> core::fmt::Debug for SshWiring<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SshWiring").finish_non_exhaustive()
    }
}
