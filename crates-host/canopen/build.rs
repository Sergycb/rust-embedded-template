//! Генерация словаря объектов CANopen из `device.toml`.
//!
//! `zencan-build` строит из описания устройства `static`-объекты узла
//! (`OD_TABLE`, `NODE_MBOX`, `NODE_STATE`) и кладёт код в `OUT_DIR`; в git
//! его нет — описание и код не могут разойтись.

use std::path::PathBuf;

/// Сохраняет ли узел объекты (`0x1010`): только в проекте с разделом
/// настроек — ему есть куда (`domain::canopen`). Пусто — проект без раздела:
/// объекта в словаре нет, и стек честно его не объявляет.
const STORAGE: &[bool] = &[
    // {%- if config == "true" %}
    true,
    // {%- endif %}
];

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=device.toml");
    let device = if STORAGE.is_empty() {
        // Ключ верхнего уровня — в начало: в конце он попал бы в последнюю
        // таблицу файла.
        let out = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("OUT_DIR не задан")?);
        let path = out.join("device.toml");
        let toml = std::fs::read_to_string("device.toml")?;
        std::fs::write(&path, format!("support_storage = false\n{toml}"))?;
        path
    } else {
        PathBuf::from("device.toml")
    };
    zencan_build::build_node_from_device_config("DEVICE", device)?;
    Ok(())
}
