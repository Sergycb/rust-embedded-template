//! Свойства этого образа: его версия.
//!
//! Здесь, а не в `bsp`: это свойство прошивки, а не платы. Плата отдаёт
//! адаптер разделов (`board.ota.flash`), а compose-site (`graph.rs`)
//! оборачивает его в [`Signed`] этой версией — защита от отката. Подпись
//! приложение не проверяет: ключ зашит в bootloader, и проверяет он
//! (`crates-cross/boot`, `adapters::ota::screen_update`).

use bsp::ota::{Partition, StatePartition};

/// Адаптер проекта с подписью — тип слота узла OTA в графе.
pub type Signed = adapters::ota::Signed<Partition, StatePartition>;

/// Версия этого образа — `version` из Cargo.toml, свёрнутая
/// `domain::ota::version::pack` в четыре байта.
///
/// Компоненты берутся из того же `shadow-rs`, что печатает версию в баннере
/// старта (`crate::build`), — у версии в прошивке один источник. Считается
/// на компиляции: версия, не влезшая в поля (`major`/`minor` до 255, `patch`
/// до 65535), — ошибка сборки, а не прошивка, молча принимающая откат.
/// Пререлизный суффикс не учитывается: semver считает `1.2.3-rc1` младше
/// `1.2.3`, а уместить это в четыре байта нечем — README, «Защита от отката».
pub const FW_VERSION: u32 = {
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

/// Версия этого образа, видимая снаружи по имени символа.
///
/// Существует ради одной вещи: `cargo xtask build` находит её в ELF и
/// дописывает те же четыре байта в хвост `app.bin`. Так у версии остаётся один
/// источник — то, что скомпилировано в прошивку, — и хост не может приписать
/// образу чужой номер, прочитав его из другого места.
///
/// `#[used]` и `no_mangle` вдвоём: первое просит оставить статик, который код
/// не читает, второе — сохранить имя, по которому его ищет xtask. Строго
/// говоря, `#[used]` обязывает только компилятор (rust-lang/rust#47384), но
/// LLVM ставит символу `SHF_GNU_RETAIN`, и в release он на месте; пропади он —
/// `cargo xtask build` не найдёт символ и остановит сборку.
#[used]
#[expect(unsafe_code, reason = "обоснование — в комментарии выше")] // имя символа нужно xtask, см. выше
#[unsafe(no_mangle)]
pub static FW_VERSION_IN_IMAGE: u32 = FW_VERSION;
