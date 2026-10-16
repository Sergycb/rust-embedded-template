//! Генерация словаря объектов CANopen из `device.toml`.
//!
//! `zencan-build` строит из описания устройства `static`-объекты узла
//! (`OD_TABLE`, `NODE_MBOX`, `NODE_STATE`) и кладёт код в `OUT_DIR`; в git
//! его нет — описание и код не могут разойтись.

use std::path::PathBuf;

/// Ответ генерации на вопрос о разделе настроек (`true`/`false`; в самом
/// репозитории шаблона — неподставленный плейсхолдер). Сохраняет ли узел
/// объекты (`0x1010`): только с разделом — ему есть куда (`domain::canopen`);
/// без него объекта в словаре нет, и стек честно его не объявляет.
const CONFIG: &str = "{{config}}";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=device.toml");
    let device = if CONFIG == "false" {
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
