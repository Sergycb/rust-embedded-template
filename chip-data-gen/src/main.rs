use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

const BEGIN_MARKER: &str = "// BEGIN GENERATED CHIP LIST";
const END_MARKER: &str = "// END GENERATED CHIP LIST";
/// Любая реальная чип-фича годится — нужна только чтобы `cargo metadata` смог
/// разрешить зависимость `embassy-stm32`: в самом `cross/Cargo.toml` вместо
/// неё стоит нерезолвящийся плейсхолдер `{{chip_feature}}`, см.
/// `PatchedRepoCopy`.
const PLACEHOLDER_CHIP_FEATURE: &str = "stm32f407ve";

fn main() -> anyhow::Result<()> {
    let repo_root = repo_root();

    let embassy_features = embassy_stm32_chip_features(&repo_root)?;
    let probe_rs_chips = probe_rs_chip_names()?;

    let mut suffixes: Vec<&str> = embassy_features
        .iter()
        .map(|feature| &feature[5..]) // strip "stm32"
        .filter(|suffix| probe_rs_chips.contains(&format!("STM32{}", probe_rs_candidate(suffix))))
        .collect();
    suffixes.sort_unstable();

    let dropped = embassy_features.len() - suffixes.len();
    println!(
        "embassy-stm32: {} чип-фич; probe-rs: {} целей; итоговый список: {} (отброшено {}, нет цели probe-rs)",
        embassy_features.len(),
        probe_rs_chips.len(),
        suffixes.len(),
        dropped
    );

    write_chip_list(&repo_root.join("chip-select.rhai"), &suffixes)?;
    println!("chip-select.rhai обновлён.");
    Ok(())
}

fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir
}

/// Временная копия всего репозитория (без `target/`/`.git`/самого
/// `chip-data-gen/`) с замененным `{{chip_feature}}` в `cross/Cargo.toml` —
/// `cargo metadata` не может разрешить зависимости, пока там стоит буквальный
/// нерезолвящийся плейсхолдер. Копируем репозиторий целиком, а не только
/// `cross/`: у `cross/bsp` путь на `domain` относительный (`../domain`),
/// поэтому структура каталогов вокруг `cross/Cargo.toml` должна совпадать с
/// реальной. Каталог удаляется при выходе из области видимости.
struct PatchedRepoCopy {
    dir: PathBuf,
}

impl PatchedRepoCopy {
    fn create(repo_root: &Path) -> anyhow::Result<Self> {
        let dir = std::env::temp_dir().join(format!("chip-data-gen-repo-{}", std::process::id()));
        if dir.exists() {
            fs::remove_dir_all(&dir)
                .context("не удалось очистить старую временную копию репозитория")?;
        }
        copy_dir_recursive(repo_root, &dir)
            .context("не удалось скопировать репозиторий во временный каталог")?;

        let manifest_path = dir.join("cross/Cargo.toml");
        let manifest = fs::read_to_string(&manifest_path)?;
        let patched = manifest.replace("{{chip_feature}}", PLACEHOLDER_CHIP_FEATURE);
        if patched == manifest {
            bail!(
                "{{{{chip_feature}}}} не найден в {} — плейсхолдер переименован в cargo-generate.toml/cross/Cargo.toml?",
                manifest_path.display()
            );
        }
        fs::write(&manifest_path, patched)?;

        Ok(Self { dir })
    }

    fn cross_manifest_path(&self) -> PathBuf {
        self.dir.join("cross/Cargo.toml")
    }
}

impl Drop for PatchedRepoCopy {
    fn drop(&mut self) {
        // Ошибку игнорируем: Drop не может её вернуть, а недочищенный
        // временный каталог не мешает следующему запуску (create()
        // сам удаляет старый перед копированием).
        drop(fs::remove_dir_all(&self.dir));
    }
}

fn copy_dir_recursive(src: &Path, dst: &Path) -> anyhow::Result<()> {
    fs::create_dir_all(dst)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let src_path = entry.path();
        let dst_path = dst.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            // target/.git — не нужны для разрешения зависимостей и могут
            // быть огромными; chip-data-gen — сам этот инструмент, копировать
            // его в свою же временную копию незачем.
            let name = entry.file_name();
            if name == "target" || name == ".git" || name == "chip-data-gen" {
                continue;
            }
            copy_dir_recursive(&src_path, &dst_path)?;
        } else {
            fs::copy(&src_path, &dst_path)?;
        }
    }
    Ok(())
}

/// Полный список объявленных чип-фич `embassy-stm32` (например `stm32f407ve`,
/// а у двухъядерных чипов — с суффиксом ядра: `stm32h745zi-cm7`), взятый из
/// его собственного `Cargo.toml` через `cargo metadata` — это декларированные
/// features пакета, а не только разрешённые для текущей сборки. Отфильтровано
/// от служебных фич без цифр/букв чипа: у тех либо нет дефиса вовсе
/// (`low-power`, `time-driver-any`), либо их несколько подряд — у чип-фич
/// дефис максимум один.
fn embassy_stm32_chip_features(repo_root: &Path) -> anyhow::Result<BTreeSet<String>> {
    let repo_copy = PatchedRepoCopy::create(repo_root)?;

    let output = Command::new("cargo")
        .args(["metadata", "--format-version", "1", "--manifest-path"])
        .arg(repo_copy.cross_manifest_path())
        .output()
        .context("не удалось запустить `cargo metadata` для cross/Cargo.toml")?;
    if !output.status.success() {
        bail!(
            "`cargo metadata` завершился с ошибкой:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let metadata: serde_json::Value = serde_json::from_slice(&output.stdout)
        .context("не удалось разобрать JSON от `cargo metadata`")?;
    let packages = metadata["packages"]
        .as_array()
        .context("`cargo metadata`: нет поля packages[]")?;
    let embassy_stm32 = packages
        .iter()
        .find(|package| package["name"] == "embassy-stm32")
        .context("embassy-stm32 не найден среди зависимостей cross/Cargo.toml")?;
    let features = embassy_stm32["features"]
        .as_object()
        .context("embassy-stm32: нет поля features")?;

    let chip_features = features
        .keys()
        .filter(|key| is_chip_feature(key))
        .cloned()
        .collect::<BTreeSet<_>>();
    if chip_features.is_empty() {
        bail!(
            "embassy-stm32: не найдено ни одной чип-фичи вида \"stm32xxxxxx\" — формат Cargo.toml изменился?"
        );
    }
    Ok(chip_features)
}

fn is_chip_feature(key: &str) -> bool {
    let Some(suffix) = key.strip_prefix("stm32") else {
        return false;
    };
    if suffix.is_empty() {
        return false;
    }
    // Не более одного дефиса — у двухъядерных чипов (`h745zi-cm7`) и
    // отдельных силиконовых градаций (`l151c6-a`) он ровно один, у
    // некоторых служебных фич — по несколько подряд, у чип-фич без
    // особенностей — ни одного.
    let mut parts = suffix.split('-');
    let is_valid_part = |s: &str| {
        !s.is_empty()
            && s.chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
    };
    match (parts.next(), parts.next(), parts.next()) {
        (Some(base), None, None) => is_valid_part(base),
        (Some(base), Some(extra), None) => is_valid_part(base) && is_valid_part(extra),
        _ => false,
    }
}

/// Часть суффикса, которую нужно сверять с `probe-rs chip list` — до первого
/// дефиса и в верхнем регистре. probe-rs не различает ядро/градацию в имени
/// цели (`STM32H745ZI` — одна цель с двумя ядрами внутри, без `-CM7`), это
/// различие есть только в фиче `embassy-stm32`, которая и определяет, какой
/// PAC-подмодуль/ядро реально соберётся.
fn probe_rs_candidate(suffix: &str) -> String {
    suffix
        .split('-')
        .next()
        .expect("split всегда возвращает хотя бы один элемент")
        .to_uppercase()
}

/// Множество идентификаторов чипов, которые знает локально установленный
/// `probe-rs` (`probe-rs chip list`), например `STM32F407VE` — без суффикса
/// корпус/темп.диапазон (`Tx`/`Hx`/...), см. обоснование в CLAUDE.md.
fn probe_rs_chip_names() -> anyhow::Result<BTreeSet<String>> {
    let output = Command::new("probe-rs")
        .args(["chip", "list"])
        .output()
        .context(
            "не удалось запустить `probe-rs chip list` — установите probe-rs-tools \
             (см. README, раздел про прошивку/отладку)",
        )?;
    if !output.status.success() {
        bail!(
            "`probe-rs chip list` завершился с ошибкой:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let chips = stdout
        .lines()
        // Строки семейств не имеют отступа, строки конкретных чипов — с
        // ведущими пробелами.
        .filter(|line| line.starts_with(char::is_whitespace))
        .map(|line| line.trim().to_string())
        .filter(|chip| chip.starts_with("STM32"))
        .collect::<BTreeSet<_>>();
    if chips.is_empty() {
        bail!("`probe-rs chip list` не вернул ни одного чипа STM32 — вывод команды изменился?");
    }
    Ok(chips)
}

fn write_chip_list(rhai_path: &Path, suffixes: &[&str]) -> anyhow::Result<()> {
    let original = std::fs::read_to_string(rhai_path)
        .with_context(|| format!("не удалось прочитать {}", rhai_path.display()))?;

    let begin = original
        .find(BEGIN_MARKER)
        .with_context(|| format!("{BEGIN_MARKER} не найден в {}", rhai_path.display()))?;
    let end = original
        .find(END_MARKER)
        .with_context(|| format!("{END_MARKER} не найден в {}", rhai_path.display()))?;
    if end < begin {
        bail!(
            "{END_MARKER} стоит раньше {BEGIN_MARKER} в {}",
            rhai_path.display()
        );
    }

    let mut generated = String::new();
    generated.push_str(BEGIN_MARKER);
    generated.push_str(" (");
    generated.push_str(&suffixes.len().to_string());
    generated.push_str(" шт., cargo run --manifest-path chip-data-gen/Cargo.toml)\n");
    generated.push_str("    const CHIPS = [\n");
    for suffix in suffixes {
        generated.push_str("        \"");
        generated.push_str(suffix);
        generated.push_str("\",\n");
    }
    generated.push_str("    ];\n    ");

    let mut updated = String::with_capacity(original.len() + generated.len());
    updated.push_str(&original[..begin]);
    updated.push_str(&generated);
    updated.push_str(&original[end..]);

    std::fs::write(rhai_path, updated)
        .with_context(|| format!("не удалось записать {}", rhai_path.display()))
}
