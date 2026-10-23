//! `cargo xtask test <этап>` — три этапа тестирования и их сумма.

use std::{env, fs};

use anyhow::Context;
use xshell::cmd;

use crate::flash::{flash_all, flash_boot};
use crate::image::raw_image;
use crate::memory::{Region, app_memory_regions, app_region, region};
use crate::project::{
    CHIP, PAGE_SIZE, TARGET, WRITE_SIZE, cross_target_dir, external_partitions, has_bootloader,
    host_features, root_dir, signing_enabled,
};
use crate::signing::{ensure_image_key, ensure_signing_key};

/// Этапы: имя, строка справки, запуск. Одна таблица и для справки, и для
/// выбора — разъехаться им негде.
const STAGES: [(&str, &str, Stage); 4] = [
    ("host", "логика domain на хосте — плата не нужна", test_host),
    (
        "target",
        "тесты внутри МК (embedded-test + probe-rs run) — нужна плата",
        test_target,
    ),
    (
        "host-target",
        "хост управляет прошитым устройством — нужна плата",
        test_host_target,
    ),
    ("all", "все три подряд", test_all),
];

type Stage = fn(&xshell::Shell) -> Result<(), anyhow::Error>;

/// Без этапа — список этапов, а не запуск какого-то из них, и код выхода
/// ненулевой: иначе `cargo xtask test` в скрипте давал бы зелёный прогон, в
/// котором не выполнилось ни одного теста.
pub fn test(sh: &xshell::Shell, stage: &[&str]) -> Result<(), anyhow::Error> {
    if let [name] = stage
        && let Some((_, _, run)) = STAGES.iter().find(|(known, ..)| known == name)
    {
        return run(sh);
    }
    println!("USAGE cargo xtask test <ЭТАП>");
    for (name, about, _) in STAGES {
        println!("      {name:<12} # {about}");
    }
    match stage {
        [] => anyhow::bail!("не указан этап тестирования"),
        other => anyhow::bail!("неизвестный этап: cargo xtask test {}", other.join(" ")),
    }
}

fn test_all(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    test_host(sh)?;
    test_target(sh)?;
    test_host_target(sh)
}

fn test_host(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    let _p = sh.push_dir(root_dir());
    let features = host_features();
    cmd!(
        sh,
        "cargo nextest run --workspace --exclude host-target-tests --release --features {features}"
    )
    .run()?;
    // nextest doc-тесты не запускает.
    cmd!(
        sh,
        "cargo test --doc --workspace --exclude host-target-tests --release --features {features}"
    )
    .run()?;
    Ok(())
}

/// Хост управляет уже прошитым устройством: сначала заливаем то, что тест
/// будет проверять, потом запускаем сам тест. Чип и адреса разделов уходят к
/// нему через окружение: первое подставляется при генерации в одном месте,
/// вторые считаются по `memory.x` — знать это в двух местах незачем.
fn test_host_target(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    // Ключ — до прошивки, как и в `build`: открытый попадает в bootloader
    // через его build.rs, то есть должен существовать к моменту компиляции.
    let seed = if signing_enabled() && has_bootloader() {
        ensure_signing_key(&root_dir())?
    } else {
        None
    };
    flash_all(sh, "release")?;
    // Как в `build`: ключ в залитом bootloader'е обязан быть тем, которым
    // подписывается инертный образ, — иначе тест обмена упал бы, обвиняя
    // bootloader в отказе, а не сборку в старом ключе.
    if let Some(seed) = &seed {
        let boot_elf = cross_target_dir().join(TARGET).join("release").join("boot");
        let (base, image) = raw_image(&boot_elf)?;
        ensure_image_key(&boot_elf, base, &image, seed)?;
    }

    let regions = app_memory_regions()?;

    // Разделы OTA — ради теста полного цикла обновления: он пишет образ в
    // `DFU`, просит смену разделов через `BOOTLOADER_STATE` и смотрит, что
    // оказалось в `ACTIVE`. Раздел приложения ищется тем же `app_region`, что и
    // в `build`: разъедься эти два места, `build` мерил бы один регион, а тест
    // проверял другой.
    let active = app_region(&regions).map(|(_, partition)| partition);
    let dfu = region(&regions, "DFU");
    let state = region(&regions, "BOOTLOADER_STATE");
    let ram = region(&regions, "RAM");

    {
        let _env_chip = sh.push_env("HOST_TARGET_CHIP", CHIP);
        let _env_ota = ota_env(sh, active, dfu, state, ram, seed.as_ref())?;
        let _p = sh.push_dir(root_dir().join("crates-host").join("host-target-tests"));
        // В один поток: пробник у платы один, а nextest по умолчанию гоняет
        // тесты параллельно — два `probe-rs` на одной цели дерутся за неё и
        // падают с невнятной ошибкой захвата.
        cmd!(sh, "cargo nextest run --test-threads 1").run()?;
    }
    Ok(())
}

/// Адреса разделов OTA и инертный образ в окружение теста — если разделы
/// вообще есть.
///
/// Возвращает guard'ы `xshell`: переменные живут, пока жив результат. Пустой
/// вектор (нет bootloader'а или региона) означает, что тест полного цикла
/// обновления сам себя пропустит — проверять там нечего.
///
/// С подписью (`seed`) рядом с образом кладётся запись о ней
/// (`ports::PendingImage`): без неё bootloader отвергает обмен до первого шага.
fn ota_env<'a>(
    sh: &'a xshell::Shell,
    active: Option<&Region>,
    dfu: Option<&Region>,
    state: Option<&Region>,
    ram: Option<&Region>,
    seed: Option<&[u8; 32]>,
) -> Result<Vec<xshell::PushEnv<'a>>, anyhow::Error> {
    // Тест пишет образ в `DFU` и запрос в `STATE` пробником, по адресам из
    // memory.x. На внешней флеши этих адресов нет, и регионов в memory.x тоже
    // — это не ошибка раскладки, говорить о ней как об ошибке нельзя.
    if external_partitions() {
        println!("тест цикла OTA пропущен: DFU и STATE на внешней флеши, пробнику их не достать");
        return Ok(Vec::new());
    }
    let (Some(active), Some(dfu), Some(state), Some(ram)) = (active, dfu, state, ram) else {
        if has_bootloader() {
            println!(
                "ВНИМАНИЕ: в memory.x не хватает регионов ACTIVE/DFU/BOOTLOADER_STATE/RAM — \
                 тест цикла OTA пропущен"
            );
        }
        return Ok(Vec::new());
    };
    // Пустой `page_size` означает, что раскладку считал не шаблон, а человек
    // (см. chip-select.rhai): размер страницы стирания тогда неизвестен, а
    // без него тест обмена искал бы прежний образ не по тому адресу — он
    // уезжает ровно на страницу. Лучше пропустить проверку, чем получить
    // падение, обвиняющее bootloader.
    if PAGE_SIZE.is_empty() {
        println!("ВНИМАНИЕ: размер страницы стирания неизвестен — тест цикла OTA пропущен");
        return Ok(Vec::new());
    }

    let tmp = env::temp_dir();
    let image = inert_image(
        u32::try_from(active.origin).context("ORIGIN(ACTIVE) не в 32 битах")?,
        u32::try_from(ram.origin + ram.length).context("конец RAM не в 32 битах")?,
    );
    let image_path = tmp.join("host-target-ota-image.bin");
    fs::write(&image_path, &image).with_context(|| format!("записать {}", image_path.display()))?;

    let mut vars = Vec::new();
    // Подписать нечем — обмен bootloader не пустит. Отказ чужому образу
    // проверяется и так: ему ключ не нужен.
    if signing_enabled() && seed.is_none() {
        println!(
            "ВНИМАНИЕ: закрытого ключа нет — обмен и откат подписанного образа НЕ \
             проверяются, только отказ неподписанному"
        );
    }
    // Без OTA модуля `ports::ota` в проекте нет (его вырезает генерация), а
    // `seed` там всегда `None`: подпись бывает только с bootloader'ом.
    // {%- if ota == "true" %}
    if let Some(seed) = seed {
        let record = ports::PendingImage {
            len: u32::try_from(image.len()).context("длина инертного образа")?,
            signature: crate::signing::signature(&image, seed),
        };
        let path = tmp.join("host-target-ota-record.bin");
        fs::write(&path, record.to_bytes())
            .with_context(|| format!("записать {}", path.display()))?;
        vars.push(sh.push_env("HOST_TARGET_SIGNATURE_RECORD", path));
    }
    // {%- endif %}
    vars.extend([
        sh.push_env("HOST_TARGET_SIGNED", signing_enabled().to_string()),
        sh.push_env("HOST_TARGET_INERT_IMAGE", image_path),
        sh.push_env("HOST_TARGET_ACTIVE_ADDR", format!("{:#x}", active.origin)),
        // Размер `ACTIVE` — это `capacity`, с которым bootloader ищет запись о
        // подписи (`DFU + capacity`), и от него же зависит, сколько длится обмен.
        sh.push_env("HOST_TARGET_ACTIVE_LEN", active.length.to_string()),
        sh.push_env("HOST_TARGET_DFU_ADDR", format!("{:#x}", dfu.origin)),
        sh.push_env("HOST_TARGET_STATE_ADDR", format!("{:#x}", state.origin)),
        // Длина нужна не для красоты: `mark_updated()` стирает раздел
        // состояния ЦЕЛИКОМ, и тест обязан делать то же самое. Раздел
        // многосекторный на чипах с мелкими страницами (журнал прогресса —
        // слово на страницу ACTIVE в каждом из четырёх проходов), и стирание
        // одного первого сектора оставило бы там прошлый журнал.
        sh.push_env("HOST_TARGET_STATE_LEN", state.length.to_string()),
        sh.push_env("HOST_TARGET_WRITE_SIZE", WRITE_SIZE),
        sh.push_env("HOST_TARGET_PAGE_SIZE", PAGE_SIZE),
    ]);
    Ok(vars)
}

/// Образ, который тест цикла OTA заливает в `DFU`: таблица векторов и `b .`.
///
/// Обязан стартовать (иначе проверялся бы `HardFault`, а не откат) и обязан НЕ
/// дойти до `mark_booted()`, иначе откатывать нечего. Стек — вершина RAM:
/// сам образ стеком не пользуется, но Cortex-M читает это слово при старте
/// раньше первой инструкции. Обработчик сброса — сразу за двумя словами
/// таблицы, бит 0 — режим Thumb, без него ядро уходит в `HardFault` на первой же
/// инструкции. Собирается здесь, а не в тесте: при подписи хеш считается от
/// тех самых байт, что лягут в `DFU`.
fn inert_image(active: u32, ram_end: u32) -> Vec<u8> {
    let mut image = Vec::new();
    image.extend_from_slice(&ram_end.to_le_bytes());
    image.extend_from_slice(&(active + 8 + 1).to_le_bytes());
    image.extend_from_slice(&0xE7FE_u16.to_le_bytes());
    image
}

/// Тесты внутри МК. Bootloader заливается первым по той же причине, что и в
/// `test_host_target`: тестовый образ линкуется в `ACTIVE` (его `memory.x` —
/// копия app'ового), а `probe-rs run` после заливки сбрасывает чип, и
/// управление получает не тест, а то, что лежит с базы flash. На плате, где
/// bootloader'а нет — свежей или после `probe-rs erase`, — без этой строки
/// не выполнился бы ни один тест.
fn test_target(sh: &xshell::Shell) -> Result<(), anyhow::Error> {
    flash_boot(sh, false)?;
    let _p = sh.push_dir(root_dir().join("crates-cross/target-tests"));
    cmd!(sh, "cargo test").run()?;
    Ok(())
}
