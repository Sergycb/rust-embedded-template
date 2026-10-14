//! Генерация словаря объектов CANopen из `device.toml`.
//!
//! `zencan-build` строит из описания устройства `static`-объекты узла
//! (`OD_TABLE`, `NODE_MBOX`, `NODE_STATE`) и кладёт код в `OUT_DIR`; в git
//! его нет — описание и код не могут разойтись.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=device.toml");
    zencan_build::build_node_from_device_config("DEVICE", "device.toml")?;
    Ok(())
}
