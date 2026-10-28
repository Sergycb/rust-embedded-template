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

/// Манифест прошивки: версия ПО узла (`0x100A`) — её `version`, та же, что
/// в образе и в сведениях об устройстве, а не версия этого крейта (он в
/// другом воркспейсе, и номера разъехались бы).
const FIRMWARE_MANIFEST: &str = "../../crates-cross/Cargo.toml";

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=device.toml");
    println!("cargo:rerun-if-changed={FIRMWARE_MANIFEST}");
    let mut toml = std::fs::read_to_string("device.toml")?;
    if let Some(version) = firmware_version()? {
        toml = toml
            .lines()
            .map(|line| {
                if line.starts_with("software_version") {
                    format!("software_version = \"{version}\"")
                } else {
                    line.to_owned()
                }
            })
            .collect::<Vec<_>>()
            .join("\n");
    }
    if CONFIG == "false" {
        // Ключ верхнего уровня — в начало: в конце он попал бы в последнюю
        // таблицу файла.
        toml = format!("support_storage = false\n{toml}");
    }
    let out = PathBuf::from(std::env::var_os("OUT_DIR").ok_or("OUT_DIR не задан")?);
    let device = out.join("device.toml");
    std::fs::write(&device, toml)?;
    zencan_build::build_node_from_device_config("DEVICE", device)?;
    Ok(())
}

/// `version` из `[workspace.package]` манифеста прошивки; `None` — манифеста
/// нет (крейт собран отдельно от проекта): тогда остаётся `device.toml`.
fn firmware_version() -> Result<Option<String>, Box<dyn std::error::Error>> {
    let Ok(manifest) = std::fs::read_to_string(FIRMWARE_MANIFEST) else {
        return Ok(None);
    };
    let version = manifest
        .lines()
        .skip_while(|line| line.trim() != "[workspace.package]")
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('['))
        .find_map(|line| {
            let (key, value) = line.split_once('=')?;
            (key.trim() == "version").then(|| value.trim().trim_matches('"').to_owned())
        })
        .ok_or("в crates-cross/Cargo.toml нет version в [workspace.package]")?;
    Ok(Some(version))
}
