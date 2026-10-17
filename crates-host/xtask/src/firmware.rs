//! `cargo xtask build` — обе сборки прошивки, сырой образ, его размер и подпись.

use std::fs;

use anyhow::Context;
use xshell::cmd;

use crate::image::{image_version, raw_image, unpack_version};
use crate::project::{
    APP_ARTIFACT, TARGET, cross_target_dir, has_bootloader, root_dir, signing_enabled,
};
use crate::signing::{ensure_image_key, ensure_signing_key, sign_image};
use crate::size::{report_boot, report_image, warn_boot_runtime_symbols};

pub fn build(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    // Ключ приводится в порядок ДО сборки, а не после: открытый попадает в
    // bootloader через build.rs крейта boot, то есть должен существовать к
    // моменту компиляции.
    //
    // И только там, где есть что подписывать: без bootloader'а раздела `DFU` не
    // существует, доставлять образ некуда, а пара ключей в корне проекта была
    // бы вредна — открытый просился бы в коммит, закрытый охранял бы пустоту.
    let seed = if signing_enabled() && has_bootloader() {
        ensure_signing_key(&root_dir())?
    } else {
        None
    };

    {
        let _p = sh.push_dir(root_dir().join("crates-cross"));
        cmd!(sh, "cargo build").run()?;
        cmd!(sh, "cargo build --release").run()?;

        // Bootloader собирается ОТДЕЛЬНЫМ вызовом, и это не про удобство.
        // Фичи зависимостей cargo объединяет по всем членам воркспейса,
        // выбранным в одном вызове: `app` просит у `embassy-stm32` фичу
        // `time-driver-any`, и в общей сборке она достаётся ещё и `boot` —
        // тот запускает TIM и оставляет его прерывание размаскированным на
        // момент прыжка в приложение. Своим вызовом `boot` получает
        // собственный резолв фич, то есть тот же образ, что заливают на
        // плату тестовые этапы (`cargo flash -p boot`) и вы руками.
        //
        // Уберёте эти две строки — ELF бутлоадера в `target/` перестанет
        // совпадать с тем, что уезжает на плату, причём молча.
        if has_bootloader() {
            cmd!(sh, "cargo build -p boot").run()?;
            cmd!(sh, "cargo build -p boot --release").run()?;
        }
    }

    let elf = cross_target_dir()
        .join(TARGET)
        .join("release")
        .join(APP_ARTIFACT);
    anyhow::ensure!(
        elf.exists(),
        "сборка прошла, но ELF по пути {} не найден. Так бывает, если каталог сборки задан \
         не переменной CARGO_TARGET_DIR, а `[build] target-dir` в .cargo/config.toml — его \
         xtask не читает",
        elf.display(),
    );
    let (base, mut image) = raw_image(&elf)?;

    // Версия — в хвост образа, и обязательно ДО подписи: устройство читает её
    // из последних четырёх байт принятого образа и отказывается ставить
    // прошивку не новее текущей (защита от атаки отката). Не будь версия
    // подписана вместе с образом, соврать ею мог бы кто угодно.
    //
    // Значение берётся из символа в ELF, а не из Cargo.toml: так у версии
    // остаётся ровно один источник — то, что скомпилировано в прошивку.
    if signing_enabled() && has_bootloader() {
        let version = image_version(&elf, base, &image)?;
        let (major, minor, patch) = unpack_version(version);
        println!("версия образа: {major}.{minor}.{patch}");
        image.extend_from_slice(&version.to_le_bytes());
    }

    // Размер печатается всегда: он одинаково полезен и проекту без OTA, где
    // приложению достаётся весь флеш. Заодно сверяется адрес, с которого образ
    // начинается.
    report_image(base, image.len() as u64)?;

    // А файл нужен только там, где есть куда его доставлять: `.bin` — это
    // ровно то, что уезжает в раздел `DFU`.
    if has_bootloader() {
        let boot_elf = cross_target_dir().join(TARGET).join("release").join("boot");
        let (boot_base, boot_image) = raw_image(&boot_elf)?;
        report_boot(boot_image.len() as u64)?;
        warn_boot_runtime_symbols(&boot_elf)?;

        let path = elf.with_extension("bin");
        fs::write(&path, &image).with_context(|| format!("записать {}", path.display()))?;
        println!("образ для OTA: {}", path.display());

        if let Some(seed) = seed {
            // Ключ проверяет bootloader — с его release-ELF и сверяем.
            ensure_image_key(&boot_elf, boot_base, &boot_image, &seed)?;
            sign_image(&path, &image, &seed)?;
        }
    }
    Ok(())
}
