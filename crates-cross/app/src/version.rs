//! Свойства образа: версия и подтверждение.
//!
//! Здесь, а не в `bsp`: это свойство прошивки, а не платы. Свои свойства
//! объявляет каждый бинарник пакета константой [`Image`] — рабочий `app`
//! (`src/main.rs`) и тестовый `app-unconfirmed` (`src/bin/app-unconfirmed.rs`)
//! — и отдаёт их в {% if graph == "true" %}`graph::spawn_all` (поле `image` `boot:`-объекта
//! [`crate::graph::Boot`]){% else %}`tasks::spawn_all`{% endif %}; библиотека их не выбирает. Узел OTA получает
//! версию (`running_version`, защита от отката) и подтверждение во входе.
//! Подпись приложение не проверяет: ключ зашит в bootloader, и проверяет он
//! (`crates-cross/boot`, `domain::ota::boot::screen_update`).

/// `version` из Cargo.toml, свёрнутая `domain::ota::version::pack` в четыре
/// байта, — версия рабочего образа.
///
/// Компоненты берутся из того же `shadow-rs`, что печатает версию в баннере
/// старта (`crate::build`), — у версии в прошивке один источник. Считается
/// на компиляции: версия, не влезшая в поля (`major`/`minor` до 255, `patch`
/// до 65535), — ошибка сборки, а не прошивка, молча принимающая откат.
/// Пререлизный суффикс не учитывается: semver считает `1.2.3-rc1` младше
/// `1.2.3`, а уместить это в четыре байта нечем — README, «Защита от отката».
pub const PACKAGE_VERSION: u32 = {
    let (Ok(major), Ok(minor), Ok(patch)) = (
        u8::from_str_radix(crate::build::PKG_VERSION_MAJOR, 10),
        u8::from_str_radix(crate::build::PKG_VERSION_MINOR, 10),
        u16::from_str_radix(crate::build::PKG_VERSION_PATCH, 10),
    ) else {
        panic!(
            "version в Cargo.toml не влезает в четыре байта образа: см. domain::ota::version::pack"
        );
    };
    domain::ota::version::pack(major, minor, patch)
};

/// Свойства образа, которые знает только его бинарник.
#[derive(Clone, Copy)]
pub struct Image {
    /// Версия для защиты от отката (`domain::ota::version::pack`): образ не
    /// новее неё узел OTA отвергает. Её же `cargo xtask build` дописывает в
    /// хвост `app.bin` — из символа `FW_VERSION_IN_IMAGE` бинарника.
    pub version: u32,
    /// Версия текстом — то, что образ называет хосту (`domain::device`{% if has_modbus == "true" %},
    /// Modbus{% endif %}) и печатает в баннере старта.
    pub text: &'static str,
    /// Образ уже подтверждён: узел OTA `mark_booted` не зовёт, и следующий
    /// сброс откатывает образ (тестовый `app-unconfirmed`). Рабочий образ —
    /// `false`: подтверждает себя сам.
    pub confirmed: bool,
}
