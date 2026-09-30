//! Свойства этого образа: его версия и ключ, которым проверяются следующие.
//!
//! Здесь, а не в `bsp`: это свойства прошивки, а не платы. Плата отдаёт
//! адаптер разделов (`board.ota.flash`), а compose-site (`graph.rs`)
//! оборачивает его в [`Signed`] этими двумя величинами — проверка отката и
//! подписи до обмена разделов.

use bsp::ota::Partition;

/// Адаптер с проверкой подписи — тип слота узла OTA в графе.
pub type Signed = adapters::ota::Signed<Partition, Partition>;

/// Версия этого образа — `version` из Cargo.toml, свёрнутая
/// `domain::firmware::pack` в четыре байта.
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
        panic!("version в Cargo.toml не влезает в четыре байта образа: см. domain::firmware::pack");
    };
    domain::firmware::pack(major, minor, patch)
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
#[allow(unsafe_code)] // имя символа нужно xtask, см. выше
#[unsafe(no_mangle)]
pub static FW_VERSION_IN_IMAGE: u32 = FW_VERSION;

/// Открытый ключ, которым проверяется подпись образа.
///
/// Не константа в исходнике, а файл `ota-public-key.bin` в корне проекта: его
/// создаёт `cargo xtask build` вместе с закрытым и приносит сюда `build.rs`
/// через `OUT_DIR`. Нули означают «ключ ещё не создан» (файла нет), и
/// `domain::update::apply_signed` отказывает, не доходя до проверки.
///
/// Статик с именем, а не `const`, по той же причине, что и
/// [`FW_VERSION_IN_IMAGE`]: `cargo xtask build` находит его в ELF и сверяет с
/// ключом, которым подписывает образ. Ровно этот статик и уходит в адаптер
/// (`Signed::new` в `graph.rs`) — значит проверена вся проводка, а не копия
/// ключа рядом с ней.
#[used]
#[allow(unsafe_code)] // имя символа нужно xtask, см. выше
#[unsafe(no_mangle)]
pub static OTA_PUBLIC_KEY_IN_IMAGE: [u8; 32] =
    *include_bytes!(concat!(env!("OUT_DIR"), "/ota-public-key.bin"));
