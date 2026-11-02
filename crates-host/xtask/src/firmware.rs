// {%- if ota == "true" %}
//! `cargo xtask build` — обе сборки прошивки, сырой образ, его размер и подпись.
// {%- else %}
//{{newline}}//! `cargo xtask build` — обе сборки прошивки, сырой образ и его размер.
// {%- endif %}

// {%- if ota == "true" %}
use std::{fs, path::Path};

use anyhow::Context;
// {%- endif %}
use xshell::cmd;

use crate::image::raw_image;
// {%- if ota == "true" %}
use crate::image::{image_version, layout_fingerprint, unpack_version};
// {%- endif %}
use crate::project::{APP_ARTIFACT, TARGET, cross_target_dir, root_dir};
// {%- if ota == "true" %}
use crate::signing::{ensure_image_key, ensure_signing_key, sign_image};
// {%- endif %}
use crate::size::{report_image, report_ram};
// {%- if ota == "true" %}
use crate::size::{report_boot, warn_boot_runtime_symbols};

/// Образ для OTA из ELF приложения: сырой образ с хвостом
/// `[раскладка u32 LE][версия u32 LE]` — то, что уезжает в `DFU` и что
/// подписывается. Собирает его [`ota_image`].
#[derive(Debug)]
pub struct OtaImage {
    /// Адрес, с которого образ начинается (`ORIGIN` раздела).
    pub base: u64,
    /// Байты образа вместе с хвостом.
    pub image: Vec<u8>,
    /// Отпечаток раскладки памяти из хвоста.
    pub layout: u32,
    /// Версия образа из хвоста.
    pub version: u32,
}
// {%- endif %}

pub fn build(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    // {%- if ota == "true" %}
    // Ключ приводится в порядок ДО сборки, а не после: открытый попадает в
    // bootloader через build.rs крейта boot, то есть должен существовать к
    // моменту компиляции.
    let seed = ensure_signing_key(&root_dir())?;
    // {%- endif %}
    // {%- if net_services contains "rest" or net_services contains "mqtt" or ssh_auth == "password" or ssh_host_key == "file" %}
    // Секреты служб — тоже до сборки: прошивка вшивает их `include_str!`.
    crate::secrets::ensure(&root_dir())?;
    // {%- endif %}

    {
        let _p = sh.push_dir(root_dir().join("crates-cross"));
        // Только рабочий бинарник: голый `cargo build` собрал бы все
        // бинарники `app`{% if ota == "true" %}, и тестовый `app-unconfirmed` (`cargo xtask test
        // host-target`) лёг бы в сборку релиза рядом с рабочим{% endif %}. Пакеты — те же
        // `default-members`, что у голого вызова, поэтому и фичи те же.
        cmd!(sh, "cargo build --bin {APP_ARTIFACT}").run()?;
        cmd!(sh, "cargo build --release --bin {APP_ARTIFACT}").run()?;
        // {%- if ota == "true" %}

        // Bootloader — только release и ОТДЕЛЬНЫМ вызовом, и это не про
        // удобство. Debug-сборки у него нет (зона рассчитана на release, с
        // подписью debug в неё не влезает), поэтому он не входит в
        // `default-members` и голые `cargo build` его не собирают. А фичи
        // зависимостей cargo объединяет по всем членам воркспейса, выбранным в
        // одном вызове: `app` просит у `bsp` фичу `time-driver` (`time-driver-any`),
        // и в общей сборке она достаётся ещё и `boot` — тот запускает TIM и
        // оставляет его прерывание размаскированным на момент прыжка в
        // приложение. Своим вызовом `boot` получает собственный резолв фич, то
        // есть тот же образ, что заливают на плату тестовые этапы
        // (`cargo flash -p boot --release`) и вы руками.
        //
        // Уберёте эту строку — ELF бутлоадера в `target/` перестанет
        // совпадать с тем, что уезжает на плату, причём молча.
        cmd!(sh, "cargo build -p boot --release").run()?;
        // {%- endif %}
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
    // {%- if ota == "true" %}
    let OtaImage {
        base,
        image,
        layout,
        version,
    } = ota_image(&elf)?;
    println!("раскладка: {layout:#010x}");
    {
        let (major, minor, patch) = unpack_version(version);
        println!("версия образа: {major}.{minor}.{patch}");
    }
    // {%- else %}
    //{{newline}}    let (base, image) = raw_image(&elf)?;
    // {%- endif %}

    // Размер печатается всегда; заодно сверяется адрес, с которого образ
    // начинается.
    report_image(base, image.len() as u64)?;
    report_ram(&elf)?;
    // {%- if ota == "true" %}

    // `.bin` — ровно то, что уезжает в раздел `DFU`.
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
    // {%- endif %}
    Ok(())
}
// {%- if ota == "true" %}

/// `cargo xtask sign` — подпись образа, уже собранного `cargo xtask build`.
///
/// Релиз подписывает отдельным шагом: закрытый ключ появляется только после
/// сборки, и `build.rs` и proc-macro зависимостей его не видят. Ключ — тот
/// же `ota-signing-key.bin`, и сверяется он с bootloader'ом так же, как в
/// `build`.
pub fn sign() -> Result<(), anyhow::Error> {
    let seed = ensure_signing_key(&root_dir())?
        .context("нет закрытого ключа ota-signing-key.bin — подписывать нечем")?;
    let release = cross_target_dir().join(TARGET).join("release");
    let path = release.join(APP_ARTIFACT).with_extension("bin");
    let image = fs::read(&path)
        .with_context(|| format!("прочитать {} — сначала cargo xtask build", path.display()))?;
    let boot_elf = release.join("boot");
    let (boot_base, boot_image) = raw_image(&boot_elf)?;
    ensure_image_key(&boot_elf, boot_base, &boot_image, &seed)?;
    sign_image(&path, &image, &seed)
}

/// Собирает [`OtaImage`] из release-ELF приложения `elf` — общее у `build` и
/// `test host-target` (рабочий образ и тестовый `app-unconfirmed`).
/// Bootloader (release-ELF `boot` рядом) уже собран: с ним сверяется
/// раскладка.
///
/// Хвост — `[раскладка u32 LE][версия u32 LE]`, и обязательно ДО подписи:
/// устройство читает его из последних восьми байт принятого образа. Раскладку
/// (отпечаток границ разделов из memory.x) оно сверяет со своей на `Finish` и
/// образ под другую раскладку отвергает до обмена; версию сравнивает со своей
/// и отказывается ставить прошивку не новее текущей (защита от атаки отката).
/// Не будь хвост подписан вместе с образом, соврать им мог бы кто угодно.
///
/// Оба значения берутся из ELF, а не из Cargo.toml и memory.x: так у них
/// остаётся ровно один источник — то, что скомпилировано в прошивку.
pub fn ota_image(elf: &Path) -> Result<OtaImage, anyhow::Error> {
    let (base, mut image) = raw_image(elf)?;
    let boot_elf = cross_target_dir().join(TARGET).join("release").join("boot");
    let layout = layout_fingerprint(elf)?;
    // Раскладку bootloader и приложение обязаны понимать одинаково
    // (`embassy-boot` в обоих ищет разделы по тем же символам), и образ под
    // одну раскладку с bootloader'ом под другую испортил бы обмен.
    let boot_layout = layout_fingerprint(&boot_elf)?;
    anyhow::ensure!(
        layout == boot_layout,
        "раскладка памяти приложения ({layout:#010x}) и bootloader'а ({boot_layout:#010x}) \
         разошлась: границы BOOTLOADER_STATE/ACTIVE/DFU в crates-cross/app/memory.x и \
         crates-cross/boot/memory.x обязаны совпадать. Поправьте memory.x и пересоберите; \
         если раскладка сменилась намеренно, новый bootloader прошивается по проводу — \
         образом по OTA её не сменить (устройство отвергнет его на Finish)",
    );
    let version = image_version(elf, base, &image)?;
    append_tail(&mut image, layout, version);
    Ok(OtaImage {
        base,
        image,
        layout,
        version,
    })
}

/// Дописывает хвост `[раскладка u32 LE][версия u32 LE]` в конец образа.
///
/// Читает его `domain::ota::update::apply` с конца: версия — на
/// `len - 4`, раскладка — перед ней. Зависимости на `domain` у xtask нет,
/// поэтому формат держит пара тестов — см. `writes_the_tail_domain_reads`.
fn append_tail(image: &mut Vec<u8>, layout: u32, version: u32) {
    image.extend_from_slice(&layout.to_le_bytes());
    image.extend_from_slice(&version.to_le_bytes());
}

#[cfg(test)]
mod tests {
    /// Хвост пишет xtask, а читает `domain`, и общего кода у них нет —
    /// значит разъехаться они могут только молча.
    ///
    /// Ловит это ПАРА тестов: здесь и `reads_the_tail_xtask_writes` в
    /// `domain::ota::update` (`crates-host/domain/src/ota/update.rs`), где
    /// стоят те же литералы. Поодиночке ни один из них расхождения не увидит —
    /// каждый проверяет свою сторону. Байты несимметричны, так что пойман
    /// будет и обмен полей местами, и смена порядка байт.
    #[test]
    fn writes_the_tail_domain_reads() {
        let mut image = vec![0xAA; 4];

        super::append_tail(&mut image, 0x5EED_1A70, 0x0102_0003);

        assert_eq!(image[..4], [0xAA; 4], "сам образ не тронут");
        assert_eq!(image[4..], [0x70, 0x1A, 0xED, 0x5E, 0x03, 0x00, 0x02, 0x01]);
    }
}
// {%- endif %}
