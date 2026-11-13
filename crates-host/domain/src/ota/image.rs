//! Свойства образа: версия и подтверждение.
//!
//! Свои свойства объявляет константой [`Image`] каждый бинарник `app`
//! (`crates-cross/app/src/main.rs`{% if test_image == "true" %} и
//! `src/bin/test-ota.rs`{% endif %}) и отдаёт в {% if graph == "true" %}`graph::spawn_all`
//! (поле `image` `boot:`-объекта графа){% else %}`tasks::spawn_all`{% endif %};
//! библиотека их не выбирает.
// {%- if test_image == "true" %}
//! Бинарников два — рабочий `app` и тестовый `test-ota`, свойства у каждого
//! свои.
// {%- endif %}
//!
//! Узел OTA получает версию (`running_version`, защита от отката) и
//! подтверждение во входе. Подпись приложение не проверяет: ключ зашит в
//! bootloader, и проверяет он (`crates-cross/boot`,
//! `domain::ota::boot::screen_update`).
//!
//! Версия для защиты от отката — `version` из Cargo.toml, свёрнутая
//! [`version::pack_str`] в четыре байта: образ не новее её узел OTA отвергает.
//! Её же `cargo xtask build` дописывает в хвост `app.bin` — из символа
//! `FW_VERSION_IN_IMAGE` бинарника: так у версии остаётся один источник.

/// Свойства образа, которые знает только его бинарник.
#[derive(Debug, Clone, Copy)]
pub struct Image {
    /// Версия для защиты от отката (`domain::ota::version::pack`): образ не
    /// новее неё узел OTA отвергает. Её же `cargo xtask build` дописывает в
    /// хвост `app.bin` — из символа `FW_VERSION_IN_IMAGE` бинарника.
    pub version: u32,
    /// Версия текстом — то, что образ называет хосту (`domain::device`) и
    /// печатает в баннере старта.
    pub text: &'static str,
    /// `true` — подтверждать не надо: узел OTA `mark_booted` не зовёт, образ
    /// остаётся неподтверждённым, и следующий сброс его откатывает.
    // {%- if test_image == "true" %}
    /// Так — у тестового `test-ota`.
    // {%- endif %}
    /// Рабочий образ — `false`: узел подтверждает его в стартовом действии.
    pub confirmed: bool,
}
