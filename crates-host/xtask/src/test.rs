//! `cargo xtask test <этап>` — три этапа тестирования и их сумма.

use xshell::cmd;

use crate::flash::{flash_all, flash_boot};
use crate::memory::{Region, app_memory_regions, app_region, region};
use crate::project::{
    CHIP, PAGE_SIZE, WRITE_SIZE, external_partitions, has_bootloader, host_features, root_dir,
};

type Stage = fn(&xshell::Shell) -> Result<(), anyhow::Error>;

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
    flash_all(sh, "release")?;

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
        let _env_ota = ota_env(sh, active, dfu, state, ram);
        let _p = sh.push_dir(root_dir().join("crates-host").join("host-target-tests"));
        // В один поток: пробник у платы один, а nextest по умолчанию гоняет
        // тесты параллельно — два `probe-rs` на одной цели дерутся за неё и
        // падают с невнятной ошибкой захвата.
        cmd!(sh, "cargo nextest run --test-threads 1").run()?;
    }
    Ok(())
}

/// Адреса разделов OTA в окружение теста — если они вообще есть.
///
/// Возвращает guard'ы `xshell`: переменные живут, пока жив результат. Пустой
/// вектор (нет bootloader'а или региона) означает, что тест полного цикла
/// обновления сам себя пропустит — проверять там нечего.
fn ota_env<'a>(
    sh: &'a xshell::Shell,
    active: Option<&Region>,
    dfu: Option<&Region>,
    state: Option<&Region>,
    ram: Option<&Region>,
) -> Vec<xshell::PushEnv<'a>> {
    // Тест пишет образ в `DFU` и запрос в `STATE` пробником, по адресам из
    // memory.x. На внешней флеши этих адресов нет, и регионов в memory.x тоже
    // — это не ошибка раскладки, говорить о ней как об ошибке нельзя.
    if external_partitions() {
        println!("тест цикла OTA пропущен: DFU и STATE на внешней флеши, пробнику их не достать");
        return Vec::new();
    }
    let (Some(active), Some(dfu), Some(state), Some(ram)) = (active, dfu, state, ram) else {
        if has_bootloader() {
            println!(
                "ВНИМАНИЕ: в memory.x не хватает регионов ACTIVE/DFU/BOOTLOADER_STATE/RAM — \
                 тест цикла OTA пропущен"
            );
        }
        return Vec::new();
    };
    // Пустой `page_size` означает, что раскладку считал не шаблон, а человек
    // (см. chip-select.rhai): размер страницы стирания тогда неизвестен, а
    // без него тест обмена искал бы прежний образ не по тому адресу — он
    // уезжает ровно на страницу. Лучше пропустить проверку, чем получить
    // падение, обвиняющее bootloader.
    if PAGE_SIZE.is_empty() {
        println!("ВНИМАНИЕ: размер страницы стирания неизвестен — тест цикла OTA пропущен");
        return Vec::new();
    }
    vec![
        sh.push_env("HOST_TARGET_ACTIVE_ADDR", format!("{:#x}", active.origin)),
        sh.push_env("HOST_TARGET_DFU_ADDR", format!("{:#x}", dfu.origin)),
        sh.push_env("HOST_TARGET_STATE_ADDR", format!("{:#x}", state.origin)),
        // Длина нужна не для красоты: `mark_updated()` стирает раздел
        // состояния ЦЕЛИКОМ, и тест обязан делать то же самое. Раздел
        // многосекторный на чипах с мелкими страницами (журнал прогресса —
        // слово на страницу ACTIVE в каждом из четырёх проходов), и стирание
        // одного первого сектора оставило бы там прошлый журнал.
        sh.push_env("HOST_TARGET_STATE_LEN", state.length.to_string()),
        // Вершина RAM — начальное значение указателя стека для образа,
        // который тест зальёт в DFU. Сам он стеком не пользуется, но
        // Cortex-M читает это слово при старте раньше первой инструкции.
        sh.push_env(
            "HOST_TARGET_RAM_END",
            format!("{:#x}", ram.origin + ram.length),
        ),
        sh.push_env("HOST_TARGET_WRITE_SIZE", WRITE_SIZE),
        sh.push_env("HOST_TARGET_PAGE_SIZE", PAGE_SIZE),
    ]
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
