//! Код, сгенерированный `zencan-build` из `device.toml`.

// Сгенерированный код линтам проекта не следует и править его нельзя;
// `allow`, а не `expect`: какие именно линты сработают, зависит от описания
// устройства и версии генератора. `unsafe_code` — генератор сам держит
// буфер SDO в `static mut` и отдаёт ссылку на него почтовому ящику один раз,
// при инициализации `static`.
#[allow(
    unsafe_code,
    clippy::all,
    clippy::pedantic,
    clippy::allow_attributes_without_reason,
    clippy::undocumented_unsafe_blocks,
    nonstandard_style,
    unused,
    missing_debug_implementations,
    missing_docs,
    reason = "код сгенерирован zencan-build из device.toml"
)]
mod generated {
    zencan_node::include_modules!(DEVICE);
}

pub use generated::*;
