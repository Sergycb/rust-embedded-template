//! Вход узла `SSH`: что раздаёт ему compose-site.

/// Вход узла `SSH`: стек, сведения, вход пользователя и из чего собрать ключ
/// хоста `K` (`Fixed` — вшитый ключ, `Stored` — над хранилищем настроек).
///
/// Написан руками, а не выведен фрагментом: слот ключа инициализируется
/// сборкой (порт [`Assemble`](ports::Assemble)), а не чтением поля.
pub struct SshWiring<K: ports::Assemble> {
    /// Сетевой стек, на котором узел открывает свой сокет.
    pub stack: crate::net::Stack<'static>,
    /// Сведения для `info`.
    pub info: crate::device::DeviceInfo,
    /// Как пускать пользователя.
    pub login: crate::ssh::Login,
    /// Из чего собрать ключ хоста.
    pub host_key: K::Source,
    // {%- if ota == "true" %}
    /// Клиент входа `CMD` узла OTA — для `reboot`.
    pub ota: crate::ota::OtaClient,
    // {%- endif %}
}

// Вручную: у стека `embassy-net` нет `Debug`.
impl<K: ports::Assemble> core::fmt::Debug for SshWiring<K> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("SshWiring").finish_non_exhaustive()
    }
}
