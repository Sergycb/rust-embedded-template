use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

const CHIPS_BEGIN: &str = "// BEGIN GENERATED CHIP LIST";
const CHIPS_END: &str = "// END GENERATED CHIP LIST";
const PACKAGES_BEGIN: &str = "// BEGIN GENERATED PACKAGE CHOICES";
const PACKAGES_END: &str = "// END GENERATED PACKAGE CHOICES";
const MEMORY_BEGIN: &str = "// BEGIN GENERATED MEMORY LAYOUT";
const MEMORY_END: &str = "// END GENERATED MEMORY LAYOUT";
const BANKS_BEGIN: &str = "// BEGIN GENERATED BANK MODE";
const BANKS_END: &str = "// END GENERATED BANK MODE";
/// Любая реальная чип-фича годится — нужна только чтобы `cargo metadata` смог
/// разрешить зависимость `embassy-stm32`: в самом `cross/Cargo.toml` вместо
/// неё стоит нерезолвящийся плейсхолдер `{{chip_feature}}`, см.
/// `PatchedRepoCopy`.
const PLACEHOLDER_CHIP_FEATURE: &str = "stm32f407ve";
/// Суффиксы после дефиса, обозначающие конкретное ядро (двухъядерные чипы) —
/// не корпусировку/градацию. Должно совпадать с `core_override()` в
/// `chip-select.rhai`.
const CORE_MARKERS: &[&str] = &["cm0", "cm0p", "cm3", "cm4", "cm7", "cm23", "cm33"];

fn main() -> anyhow::Result<()> {
    let repo_root = repo_root();

    let cargo_metadata = resolve_cargo_metadata(&repo_root)?;
    let probe_rs_chips = probe_rs_chip_names()?;

    let mut suffixes: Vec<&str> = cargo_metadata
        .embassy_chip_features
        .iter()
        .map(|feature| &feature[5..]) // strip "stm32"
        .filter(|suffix| probe_rs_chips.contains(&format!("STM32{}", probe_rs_candidate(suffix))))
        .collect();
    suffixes.sort_unstable();

    let dropped = cargo_metadata.embassy_chip_features.len() - suffixes.len();

    let package_choices: BTreeMap<&str, Vec<String>> = suffixes
        .iter()
        .filter_map(|suffix| {
            let candidates = package_candidates(suffix, &probe_rs_chips);
            (!candidates.is_empty()).then_some((*suffix, candidates))
        })
        .collect();

    let mut memory_layouts: BTreeMap<&str, MemoryLayout> = BTreeMap::new();
    let mut bank_modes: BTreeMap<&str, &'static str> = BTreeMap::new();
    for suffix in &suffixes {
        // Ошибка разбора метаданных — не «чип не подошёл», а сломанное
        // предположение о формате `stm32-metapac`. Раньше она глушилась через
        // `.ok()`, и ~200 чипов молча исчезали из генерации при смене формата;
        // теперь падаем громко.
        let configs = parse_chip_memory(&cargo_metadata.stm32_metapac_chips_dir, suffix)
            .with_context(|| format!("чип {suffix}"))?;
        let (regions, bank_mode) =
            select_memory_config(configs).with_context(|| format!("чип {suffix}"))?;
        if !bank_mode.is_empty() {
            bank_modes.insert(suffix, bank_mode);
        }
        if let Some(layout) = compute_memory_layout(&regions) {
            memory_layouts.insert(suffix, layout);
        }
    }
    let with_ota = memory_layouts.values().filter(|m| m.ota.is_ok()).count();

    println!(
        "embassy-stm32: {} чип-фич; probe-rs: {} целей; итоговый список: {} (отброшено {}, \
         нет цели probe-rs); более точная цель probe-rs, чем базовая, найдена для {} чипов; \
         выбор банковой схемы нужен {} чипам; memory.x посчитан для {} из {} чипов, из них \
         с OTA {} (остальные — один образ на весь flash, без cross/boot)",
        cargo_metadata.embassy_chip_features.len(),
        probe_rs_chips.len(),
        suffixes.len(),
        dropped,
        package_choices.len(),
        bank_modes.len(),
        memory_layouts.len(),
        suffixes.len(),
        with_ota,
    );

    let rhai_path = repo_root.join("chip-select.rhai");
    let stamp = format_source_stamp(&declared_embassy_version(&repo_root)?, &probe_rs_version()?);
    write_generated_block(
        &rhai_path,
        CHIPS_BEGIN,
        CHIPS_END,
        &format_chip_list(&suffixes, &stamp),
    )?;
    write_generated_block(
        &rhai_path,
        PACKAGES_BEGIN,
        PACKAGES_END,
        &format_package_choices(&package_choices),
    )?;
    write_generated_block(
        &rhai_path,
        BANKS_BEGIN,
        BANKS_END,
        &format_bank_modes(&bank_modes),
    )?;
    write_generated_block(
        &rhai_path,
        MEMORY_BEGIN,
        MEMORY_END,
        &format_memory_layouts(&memory_layouts),
    )?;
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
        strip_liquid_from_manifests(&dir)?;

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

/// Убирает Liquid-условия (`{% if ... %}...{% endif %}`) из всех `Cargo.toml`
/// временной копии, выбирая ЛОЖНУЮ ветку — то есть выбрасывая блок целиком.
///
/// Нужно, потому что `cargo metadata` читает манифесты всех членов workspace,
/// а те содержат условные куски (`"dual-core"` в `cross/bsp`/`cross/boot`,
/// `"boot"` среди `members` и фича банка в `cross/Cargo.toml`) — с ними это
/// не TOML. Ложная ветка безопаснее истинной: она оставляет манифест
/// минимальным, а всё, что нужно генератору (`embassy-stm32` и его
/// `stm32-metapac`), объявлено безусловно.
fn strip_liquid_from_manifests(dir: &Path) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            strip_liquid_from_manifests(&path)?;
        } else if entry.file_name() == "Cargo.toml" {
            let original = fs::read_to_string(&path)?;
            let stripped = strip_liquid(&original);
            if stripped != original {
                fs::write(&path, stripped)?;
            }
        }
    }
    Ok(())
}

/// Вложенных `{% if %}` в шаблоне нет (и не должно появиться — проверяется
/// тестом `manifests_have_no_nested_liquid_ifs`), поэтому поиск ближайшего
/// `{% endif %}` корректен.
fn strip_liquid(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(start) = rest.find("{% if") {
        out.push_str(&rest[..start]);
        let after = &rest[start..];
        let Some(end) = after.find("{% endif %}") else {
            // Незакрытый тег — оставляем как есть, пусть падает `cargo
            // metadata` с понятной ошибкой, а не мы с невнятной.
            break;
        };
        rest = &after[end + "{% endif %}".len()..];
    }
    out.push_str(rest);
    out
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

/// Результат разбора `cargo metadata` для пропатченного `cross/Cargo.toml`:
/// всё, что нужно взять из графа зависимостей за один вызов `cargo`.
struct CargoMetadata {
    /// Полный список объявленных чип-фич `embassy-stm32` (например
    /// `stm32f407ve`, а у двухъядерных чипов и силиконовых градаций — с
    /// суффиксом через дефис: `stm32h745zi-cm7`, `stm32l151c6-a`) — это
    /// декларированные features пакета, а не только разрешённые для текущей
    /// сборки. Отобраны только фичи вида `stm32` + строчные буквы/цифры,
    /// максимум с одним дефисом-разделителем (у всех прочих фич пакета либо
    /// нет префикса `stm32`, либо есть небуквенные символы помимо дефиса).
    embassy_chip_features: BTreeSet<String>,
    /// Каталог `src/chips` в исходниках `stm32-metapac` (той версии, что
    /// реально разрешилась для `embassy-stm32`) — там лежит подкаталог на
    /// каждый чип с его `metadata.rs` (адреса/размеры flash и RAM,
    /// см. `parse_chip_memory`). `embassy-stm32` уже зависит от него с
    /// фичей `metadata` — отдельно эту зависимость никуда добавлять не
    /// нужно, только найти, куда Cargo её скачал.
    stm32_metapac_chips_dir: PathBuf,
}

fn resolve_cargo_metadata(repo_root: &Path) -> anyhow::Result<CargoMetadata> {
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
    let embassy_chip_features = features
        .keys()
        .filter(|key| is_chip_feature(key))
        .cloned()
        .collect::<BTreeSet<_>>();
    if embassy_chip_features.is_empty() {
        bail!(
            "embassy-stm32: не найдено ни одной чип-фичи вида \"stm32xxxxxx\" — формат Cargo.toml изменился?"
        );
    }

    let stm32_metapac = packages
        .iter()
        .find(|package| package["name"] == "stm32-metapac")
        .context("stm32-metapac не найден среди зависимостей cross/Cargo.toml")?;
    let manifest_path = stm32_metapac["manifest_path"]
        .as_str()
        .context("stm32-metapac: нет поля manifest_path")?;
    let stm32_metapac_chips_dir = Path::new(manifest_path)
        .parent()
        .context("stm32-metapac: manifest_path без родительского каталога")?
        .join("src/chips");
    if !stm32_metapac_chips_dir.is_dir() {
        bail!(
            "stm32-metapac: {} не найден — структура крейта изменилась?",
            stm32_metapac_chips_dir.display()
        );
    }

    Ok(CargoMetadata {
        embassy_chip_features,
        stm32_metapac_chips_dir,
    })
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

/// Для чипов с суффиксом-градацией (не ядром — те решает `core_override()` в
/// самом `chip-select.rhai`, сюда не доходят) типа `l151c6-a` — базовая цель
/// probe-rs (`STM32L151C6`) не всегда самая точная: у пробы отдельно бывают
/// корпусировки вроде `STM32L151C6TxA`/`STM32L151C6UxA` с другим объёмом RAM.
/// Возвращает все такие цели (длиннее базовой, начинаются на базу и
/// заканчиваются буквой градации) — пусто, если их нет (значит, точнее
/// базовой цели ничего нет) или суффикс без дефиса/с ядерным маркером.
fn package_candidates(suffix: &str, probe_rs_chips: &BTreeSet<String>) -> Vec<String> {
    let Some((_, marker)) = suffix.split_once('-') else {
        return Vec::new();
    };
    if CORE_MARKERS.contains(&marker) {
        return Vec::new();
    }

    let bare = format!("STM32{}", probe_rs_candidate(suffix));
    let marker = marker.to_uppercase();
    // probe_rs_chips — BTreeSet, порядок итерации уже лексикографический;
    // отдельная сортировка не нужна.
    probe_rs_chips
        .iter()
        .filter(|chip| {
            chip.len() > bare.len() && chip.starts_with(&bare) && chip.ends_with(&marker)
        })
        .cloned()
        .collect()
}

/// Один регион памяти чипа — как в `stm32-metapac`, но только то, что нужно
/// для `memory.x`: имя (по нему `embassy-stm32` отличает одно- и двухбанковую
/// конфигурацию, см. `select_memory_config`), адрес, размер и (для flash)
/// реальный размер стирания и минимальную порцию записи.
struct RawRegion {
    name: String,
    kind: RegionKind,
    address: u64,
    size: u64,
    /// `Some` только для flash-регионов (`settings: Some(FlashSettings {..})`
    /// в исходнике `stm32-metapac`); у RAM/EEPROM-регионов — `None`.
    erase_write_size: Option<(u64, u64)>,
}

#[derive(PartialEq, Eq)]
enum RegionKind {
    Flash,
    Ram,
    /// L0/L1 — блок EEPROM, читается напрямую, пишется через контроллер flash.
    Eeprom,
    Other,
}

/// Разбирает `<stm32_metapac_chips_dir>/<chip>/metadata.rs` — это обычный
/// Rust-файл с литералом `pub static METADATA: Metadata = Metadata { ... }`,
/// сгенерированный из `stm32-data`, не то, что компилируется под конкретную
/// фичу: можно читать текстом для любого чипа без сборки.
///
/// Возвращает ВСЕ варианты карты памяти: поле `memory` — срез срезов, и у
/// чипов, умеющих и одно-, и двухбанковый режим (G4 cat3, L4+, F7, F4
/// dual-bank, L5, G0 — около 200 штук), их два. Формат при этом различается:
/// у одноконфигурационных чипов `stm32-data` печатает `memory: &[&[` одной
/// строкой, у многоконфигурационных — `memory: &[` и `&[` на следующей.
/// Поиск по литералу `memory: &[&[` (как было раньше) на вторых молча падал,
/// и чип целиком выпадал из генерации.
fn parse_chip_memory(chips_dir: &Path, suffix: &str) -> anyhow::Result<Vec<Vec<RawRegion>>> {
    // stm32-metapac каталоги — с полным именем чипа ("stm32f407ve"), а не
    // просто суффиксом ("f407ve") без префикса, как в CHIPS/PACKAGE_CHOICES.
    let path = chips_dir.join(format!("stm32{suffix}")).join("metadata.rs");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("не удалось прочитать {}", path.display()))?;

    let configs = split_memory_configs(&text)
        .with_context(|| format!("не удалось найти карту памяти в {}", path.display()))?
        .into_iter()
        .map(parse_regions)
        .collect::<anyhow::Result<Vec<_>>>()
        .with_context(|| format!("не удалось разобрать {}", path.display()))?;

    if configs.is_empty() || configs.iter().all(Vec::is_empty) {
        bail!("{}: не найдено ни одного MemoryRegion", path.display());
    }
    Ok(configs)
}

/// Тело каждого элемента внешнего среза `memory: &[ &[..], &[..] ]` — как
/// текст, без разбора самих регионов. Считает квадратные скобки, потому что
/// форматирование `stm32-data` для одно- и многоконфигурационных чипов
/// разное (см. `parse_chip_memory`), а на количество вложенных `&[` не
/// завязывается вовсе.
fn split_memory_configs(text: &str) -> anyhow::Result<Vec<&str>> {
    let start = text.find("memory: &[").context("`memory: &[` не найден")?;
    // Внешняя `[` уже съедена, поэтому глубина 0 — это «внутри memory».
    let body = &text[start + "memory: &[".len()..];

    let mut configs = Vec::new();
    let mut depth = 0i32;
    let mut config_start = None;
    let mut in_string = false;
    for (i, ch) in body.char_indices() {
        // Имена регионов (`name: "BANK_1"`) скобок не содержат, но полагаться
        // на это незачем — строки пропускаются явно. Экранированных кавычек в
        // именах регионов не бывает, поэтому и обработки `\"` здесь нет.
        if ch == '"' {
            in_string = !in_string;
            continue;
        }
        if in_string {
            continue;
        }
        match ch {
            '[' => {
                if depth == 0 {
                    config_start = Some(i + 1);
                }
                depth += 1;
            }
            ']' if depth > 0 => {
                depth -= 1;
                if depth == 0 {
                    let from = config_start
                        .take()
                        .context("закрывающая `]` без открывающей")?;
                    configs.push(&body[from..i]);
                }
            }
            // `]` на нулевой глубине — конец самого `memory`.
            ']' => break,
            _ => {}
        }
    }
    if configs.is_empty() {
        bail!("внутри `memory: &[` не найдено ни одной конфигурации");
    }
    Ok(configs)
}

fn parse_regions(section: &str) -> anyhow::Result<Vec<RawRegion>> {
    section
        .split("MemoryRegion {")
        .skip(1)
        .map(|block| {
            let name = field(block, "name")
                .context("MemoryRegion без name")?
                .trim_matches('"')
                .to_string();
            let kind = match field(block, "kind").context("MemoryRegion без kind")? {
                s if s.ends_with("Flash") => RegionKind::Flash,
                s if s.ends_with("Ram") => RegionKind::Ram,
                s if s.ends_with("Eeprom") => RegionKind::Eeprom,
                _ => RegionKind::Other,
            };
            let address_str = field(block, "address").context("MemoryRegion без address")?;
            let address = u64::from_str_radix(address_str.trim_start_matches("0x"), 16)
                .with_context(|| format!("не число: address = {address_str}"))?;
            let size: u64 = field(block, "size")
                .context("MemoryRegion без size")?
                .parse()
                .context("не число: size")?;
            let erase_write_size = if block.contains("settings: None") {
                None
            } else {
                let erase_size: u64 = field(block, "erase_size")
                    .context("FlashSettings без erase_size")?
                    .parse()
                    .context("не число: erase_size")?;
                let write_size: u64 = field(block, "write_size")
                    .context("FlashSettings без write_size")?
                    .parse()
                    .context("не число: write_size")?;
                Some((erase_size, write_size))
            };
            Ok(RawRegion {
                name,
                kind,
                address,
                size,
                erase_write_size,
            })
        })
        .collect()
}

/// Повторяет выбор карты памяти из `build.rs` самого `embassy-stm32` (там же
/// и предикаты по именам регионов): при одной конфигурации фича не нужна, при
/// нескольких — обязательна, иначе его build-скрипт паникует с «Chip supports
/// single and dual bank configuration. No Cargo feature to select one is
/// enabled». Именно поэтому ~200 чипов не собирались из шаблона вовсе, а не
/// только оставались без `memory.x`.
///
/// Из двух режимов выбирается одобанковый: он даёт непрерывный ACTIVE и не
/// требует от пользователя понимания, что такое банки. Двухбанковый нужен
/// ради read-while-write, которым `cross/boot` не пользуется.
fn select_memory_config(
    configs: Vec<Vec<RawRegion>>,
) -> anyhow::Result<(Vec<RawRegion>, &'static str)> {
    let has = |regions: &[RawRegion], bank: &str| regions.iter().any(|r| r.name.contains(bank));

    if configs.len() == 1 {
        let only = configs
            .into_iter()
            .next()
            .expect("длина проверена строкой выше");
        return Ok((only, ""));
    }

    let mut single = None;
    let mut dual = None;
    for regions in configs {
        if has(&regions, "BANK_1") && !has(&regions, "BANK_2") {
            single.get_or_insert(regions);
        } else if has(&regions, "BANK_1") && has(&regions, "BANK_2") {
            dual.get_or_insert(regions);
        }
    }
    match (single, dual) {
        (Some(regions), _) => Ok((regions, "single-bank")),
        (None, Some(regions)) => Ok((regions, "dual-bank")),
        (None, None) => bail!(
            "несколько карт памяти, но ни одна не опознана как одно-/двухбанковая \
             (embassy-stm32 такой чип тоже не соберёт)"
        ),
    }
}

/// Значение поля `{key}: ...` до ближайшей запятой/переноса строки внутри
/// одного `block` (текст между `MemoryRegion {` и следующим таким же
/// заголовком, см. `parse_chip_memory`) — этого достаточно, все нужные поля
/// здесь плоские (`name: "..."`, `address: 0x...`, `size: ...`), кроме
/// вложенного `FlashSettings { erase_size: ..., write_size: ... }`, у
/// которого поля тоже плоские и не пересекаются по имени с полями снаружи.
fn field<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("{key}: ");
    let start = block.find(&pat)? + pat.len();
    let rest = &block[start..];
    let end = rest.find([',', '\n'])?;
    Some(rest[..end].trim())
}

/// Партиции OTA-схемы (`cross/boot`). Есть только у чипов, где схема
/// физически помещается — см. `compute_ota_partitions`.
#[derive(Debug)]
struct OtaPartitions {
    /// `FLASH` в `cross/boot/memory.x`: место под сам бинарник bootloader'а,
    /// от базы flash до `BOOTLOADER_STATE`. Отдельный регион (а не весь чип)
    /// нужен, чтобы переполнение ловил линкер, а не молчаливое наложение на
    /// `ACTIVE`.
    bootloader_length: u64,
    bootloader_state_origin: u64,
    bootloader_state_length: u64,
    active_origin: u64,
    active_length: u64,
    dfu_origin: u64,
    dfu_length: u64,
}

/// Готовые для `memory.x` адреса/размеры одного чипа. Всё в байтах —
/// `format_memory_layouts` переводит в hex/`K`-нотацию для Rhai.
struct MemoryLayout {
    flash_origin: u64,
    flash_length: u64,
    ram_origin: u64,
    ram_length: u64,
    /// `None` у чипов, где RAM слишком мал, чтобы отрезать от него фиксированный
    /// кусок под `PERSIST`.
    persist: Option<(u64, u64)>,
    /// Вторая половина того же куска — под дамп `panic-persist`. `None` там же,
    /// где `None` у `persist`: либо оба региона есть, либо ни одного.
    panic: Option<(u64, u64)>,
    /// Готовые строки `MEMORY { }` для всех прочих регионов чипа (ITCM, AXISRAM,
    /// CCMRAM, BKPSRAM, EEPROM, OTP, окна внешних шин...) — см.
    /// `extra_region_lines`. Одинаковы для `app` и `boot`.
    extra_regions: Vec<String>,
    /// Const generic `BootLoader::prepare::<_, _, _, N>` — размер буфера
    /// подкачки. Требования к нему (`prepare_boot` в `embassy-boot`):
    /// делит `PAGE_SIZE`, кратен `NorFlash::WRITE_SIZE` и не меньше него.
    /// `NorFlash::WRITE_SIZE` чипа удовлетворяет всем трём, его и берём.
    /// Имя плейсхолдера в шаблоне (`write_size`) осталось прежним ради
    /// совместимости с проектами, задающими его через `--define`.
    write_size: u64,
    /// `Ok` — OTA-схема помещается в flash; `Err` — не помещается, и текст
    /// объясняет почему (печатается при генерации и попадает комментарием в
    /// `memory.x`). Во втором случае проект собирается одним образом на весь
    /// flash, без `cross/boot`.
    ota: Result<OtaPartitions, String>,
}

const FLASH_BASE: u64 = 0x0800_0000;
const RAM_BASE: u64 = 0x2000_0000;
/// Регионы, которые `stm32-metapac` перечисляет как окна ВНЕШНИХ шин: реальной
/// памяти за ними нет, пока к плате не припаяна микросхема и не настроен
/// контроллер. Метка видна и по размеру — у всех у них он одинаковый,
/// 262144000 (не память, а максимум адресного окна). Такие регионы попадают в
/// `memory.x` закомментированными: перечислить их полезно, а объявить рабочей
/// памятью — значит подсунуть линкеру то, чего на плате может не быть вовсе.
const EXTERNAL_BUS_PREFIXES: &[&str] =
    &["FMC_", "SDRAM_", "OCTOSPI_", "QUADSPI_", "HSPI_", "XSPI_"];
/// Суффикс второго (I-Code) окна того же физического блока RAM: `CCMRAM_ICODE`
/// — та же память, что `CCMRAM_DCODE`, `SRAM2_ICODE` — та же, что `SRAM2`.
/// Тоже закомментированы: разместить данные в обоих окнах разом — молчаливое
/// наложение, которое линкер не поймает (адреса-то разные).
const ALIAS_SUFFIX: &str = "_ICODE";
/// Ориентир на размер бинарника bootloader (embassy-boot + defmt +
/// cortex-m-rt) — на практике укладывается в 20-30 KiB, отсюда небольшой
/// запас. Bootloader сам по себе не стирается во время работы, поэтому ему
/// не нужно быть кратным PAGE_SIZE — только BOOTLOADER_STATE после него.
const BOOTLOADER_TARGET: u64 = 32 * 1024;
/// Сколько от зарезервированного блока обязано остаться под сам бинарник
/// после того, как из него вычли `BOOTLOADER_STATE`. Меньше — резерв растёт
/// ещё на страницу.
const BOOTLOADER_MIN: u64 = 16 * 1024;
/// На сколько страниц резерв может вырасти СВЕРХ исходного (`BOOTLOADER_TARGET`,
/// округлённого вверх до страницы). Именно сверх, а не всего: у чипа с мелкими
/// секторами исходный резерв сам по себе занимает десятки страниц (32 KiB при
/// секторе 256 байт у L0/L1 — 128 штук), и абсолютный предел отрезал бы от OTA
/// всю мелкосекторную половину линейки.
const MAX_EXTRA_RESERVED_PAGES: u64 = 16;
/// Чипы с совсем маленьким RAM: отрезать 1 KiB под `PERSIST`+`PANIC` от 4 KiB
/// уже разорительно — оба региона просто не выводятся.
const PERSIST_MIN_RAM: u64 = 4 * 1024;
/// Отрезается от конца RAM и делится пополам: `PERSIST` — под данные, которые
/// программа сама решила сохранить между сбросами (секция `.persist`), `PANIC`
/// — под дамп `panic-persist`. Разными регионами, а не одним: `panic-persist`
/// пишет по голым адресам `_panic_dump_start.._panic_dump_end`, ничего не зная
/// о секциях, и в общем регионе он затирал бы пользовательские данные — молча,
/// потому что линкеру нечего тут ловить.
const PERSIST_LENGTH: u64 = 1024;
/// Из них под дамп паники. 8 байт занимает заголовок (магия + длина),
/// остальное — текст сообщения; что не влезло, `panic-persist` обрезает.
const PANIC_LENGTH: u64 = 512;

/// Собирает карту памяти чипа: непрерывную flash-цепочку от базы, RAM-цепочку
/// от базы и — если помещается — партиции OTA. `None` только когда считать не
/// из чего (нет flash- или RAM-цепочки от базового адреса); тогда
/// `chip-select.rhai` оставляет `memory.x` плейсхолдером, как раньше.
fn compute_memory_layout(regions: &[RawRegion]) -> Option<MemoryLayout> {
    let mut flash: Vec<&RawRegion> = regions
        .iter()
        .filter(|r| r.kind == RegionKind::Flash && r.erase_write_size.is_some())
        .collect();
    flash.sort_by_key(|r| r.address);

    let mut chain: Vec<&RawRegion> = Vec::new();
    let mut end = FLASH_BASE;
    for region in flash {
        if region.address == end {
            end += region.size;
            chain.push(region);
        } else if region.address > end {
            break;
        }
    }
    let flash_total = end - FLASH_BASE;
    if chain.is_empty() || flash_total == 0 {
        return None;
    }
    // PAGE_SIZE в терминах embassy-boot — не наш const generic, а
    // `max(ACTIVE::ERASE_SIZE, DFU::ERASE_SIZE)` (`boot_loader.rs`). Обе
    // партиции живут поверх цельного `Flash` (`cross/boot/src/main.rs`), у
    // которого `NorFlash::ERASE_SIZE = MAX_ERASE_SIZE` всего чипа — отсюда
    // максимум по цепочке, а не erase_size конкретного региона.
    let page_size = chain
        .iter()
        .filter_map(|r| r.erase_write_size)
        .map(|(erase, _)| erase)
        .max()?;
    let write_size = chain[0].erase_write_size?.1;
    if page_size == 0 || write_size == 0 {
        return None;
    }

    let mut ram: Vec<&RawRegion> = regions
        .iter()
        .filter(|r| r.kind == RegionKind::Ram)
        .collect();
    ram.sort_by_key(|r| r.address);
    let mut ram_end = RAM_BASE;
    for region in ram {
        if region.address == ram_end {
            ram_end += region.size;
        } else if region.address > ram_end {
            break;
        }
    }
    let ram_total = ram_end - RAM_BASE;
    if ram_total == 0 {
        return None;
    }
    // Отрезанный кусок делится на две части: сначала PERSIST (данные
    // программы), за ним PANIC (дамп паники) — до самого конца RAM.
    let (ram_length, persist, panic) = if ram_total >= PERSIST_MIN_RAM {
        let ram_length = ram_total - PERSIST_LENGTH;
        let persist_length = PERSIST_LENGTH - PANIC_LENGTH;
        (
            ram_length,
            Some((RAM_BASE + ram_length, persist_length)),
            Some((RAM_BASE + ram_length + persist_length, PANIC_LENGTH)),
        )
    } else {
        (ram_total, None, None)
    };

    Some(MemoryLayout {
        flash_origin: FLASH_BASE,
        flash_length: flash_total,
        ram_origin: RAM_BASE,
        ram_length,
        persist,
        panic,
        // Границы цепочек, а не ram_length: PERSIST отрезан от RAM, но лежит
        // внутри той же цепочки — регионом его дублировать не надо.
        extra_regions: extra_region_lines(regions, FLASH_BASE + flash_total, ram_end),
        write_size,
        ota: compute_ota_partitions(&chain, flash_total, page_size, write_size),
    })
}

/// Готовая строка региона для `MEMORY { }`. Ширина поля имени — 18, как у
/// строк, которые собирает сам хук (`FLASH`/`ACTIVE`/`RAM`/...), чтобы
/// сгенерированный `memory.x` выглядел одним столбиком.
fn region_line(name: &str, attrs: &str, origin: u64, length: u64) -> String {
    format!(
        "    {name:<18}{:<6}: ORIGIN = {}, LENGTH = {}",
        format!("({attrs})"),
        format_addr(origin),
        format_size(length)
    )
}

/// Все регионы памяти чипа, КРОМЕ покрытых `FLASH` и `RAM` (те строит хук:
/// у `app` и `boot` они разные). Раньше не выводились вовсе, и пользователь
/// не видел, например, 320 KiB AXISRAM у H723 — только 128 KiB DTCM.
///
/// Часть регионов выводится закомментированной (см. `EXTERNAL_BUS_PREFIXES` и
/// `ALIAS_SUFFIX`): перечислить их надо, но объявлять рабочей памятью нельзя.
fn extra_region_lines(regions: &[RawRegion], flash_end: u64, ram_end: u64) -> Vec<String> {
    let names: BTreeSet<&str> = regions.iter().map(|r| r.name.as_str()).collect();
    let covered = |r: &RawRegion| {
        let end = r.address + r.size;
        (r.address >= FLASH_BASE && end <= flash_end) || (r.address >= RAM_BASE && end <= ram_end)
    };

    let mut sorted: Vec<&RawRegion> = regions
        .iter()
        .filter(|r| r.size > 0 && !covered(r))
        .collect();
    sorted.sort_by_key(|r| (r.address, r.name.clone()));

    sorted
        .iter()
        .map(|r| {
            let attrs = match r.kind {
                RegionKind::Flash => "rx",
                RegionKind::Ram => "xrw",
                // EEPROM пишется через контроллер flash, а не обычной записью,
                // поэтому без `w`: класть туда `.data` нельзя.
                RegionKind::Eeprom | RegionKind::Other => "r",
            };
            let line = region_line(&r.name, attrs, r.address, r.size);

            let reason = if EXTERNAL_BUS_PREFIXES
                .iter()
                .any(|prefix| r.name.starts_with(prefix))
            {
                Some(
                    "внешняя шина: памяти нет, пока она не распаяна и не настроен контроллер"
                        .to_string(),
                )
            } else {
                r.name.strip_suffix(ALIAS_SUFFIX).and_then(|stem| {
                    // Партнёр — либо тот же блок без суффикса (SRAM2_ICODE /
                    // SRAM2), либо через D-Code (CCMRAM_ICODE / CCMRAM_DCODE).
                    let partner = [stem.to_string(), format!("{stem}_DCODE")]
                        .into_iter()
                        .find(|name| names.contains(name.as_str()))?;
                    Some(format!("второе окно того же блока, что {partner}"))
                })
            };

            match reason {
                Some(reason) => format!("    /* {} - {reason} */", line.trim()),
                None => line,
            }
        })
        .collect()
}

/// Подбирает `BOOTLOADER`/`BOOTLOADER_STATE`/`ACTIVE`/`DFU` под все четыре
/// проверки `assert_partitions` (`embassy-boot`, `boot_loader.rs`) — это
/// runtime-assert'ы, а не документация: не выполнены — bootloader паникует
/// на старте, а не отказывается собираться.
///
/// 1. `ACTIVE` кратен `PAGE_SIZE`;
/// 2. `DFU` кратен `PAGE_SIZE`;
/// 3. `DFU - ACTIVE >= PAGE_SIZE` — запас алгоритма swap;
/// 4. `2 + 4 * (ACTIVE / PAGE_SIZE) <= STATE / NorFlash::WRITE_SIZE` — в
///    `BOOTLOADER_STATE` лежит журнал прогресса swap: по слову на страницу
///    ACTIVE в каждом из четырёх проходов плюс два служебных.
///
/// Четвёртая и есть причина, по которой `BOOTLOADER_STATE` нельзя делать
/// «просто одним сектором»: на чипе с мелкими секторами и крупным flash
/// (L4, 1 MiB при секторе 2 KiB) журналу нужно `(2 + 4*247) * 8` байт, то
/// есть четыре сектора, а не один. Раньше здесь всегда стоял ровно один
/// сектор — и 279 из 972 посчитанных чипов давали bootloader, падающий в
/// панику при первом же запуске.
///
/// Сам `BOOTLOADER` и `BOOTLOADER_STATE` кратности `PAGE_SIZE` не подчиняются
/// (в `assert_partitions` их нет) и режутся по локальной границе сектора —
/// иначе на F4/F7/H7 (сектор до 128 KiB) один отступ съедал бы четверть чипа.
///
/// `Err` — схема не помещается; текст объясняет, чего именно не хватило, и
/// доходит до пользователя при генерации.
fn compute_ota_partitions(
    chain: &[&RawRegion],
    flash_total: u64,
    page_size: u64,
    write_size: u64,
) -> Result<OtaPartitions, String> {
    let erase_size_at = |offset_from_flash_base: u64| -> Option<u64> {
        let addr = FLASH_BASE + offset_from_flash_base;
        chain
            .iter()
            .find(|r| addr >= r.address && addr < r.address + r.size)
            .and_then(|r| r.erase_write_size)
            .map(|(erase, _)| erase)
    };
    // Размеры секторов бывают и меньше килобайта (L0/L1 — 256 байт), поэтому
    // не просто деление на 1024: иначе в сообщении о причине оказался бы
    // "сектор 0 KiB".
    let size = |v: u64| {
        if v >= 1024 {
            format!("{} KiB", v / 1024)
        } else {
            format!("{v} B")
        }
    };

    let initial_pages = BOOTLOADER_TARGET.div_ceil(page_size).max(1);
    let mut reserved_pages = initial_pages;
    loop {
        if reserved_pages > initial_pages + MAX_EXTRA_RESERVED_PAGES {
            return Err(format!(
                "резерв под bootloader не подобрался, дорастив его на {MAX_EXTRA_RESERVED_PAGES} страниц по {}",
                size(page_size)
            ));
        }
        let reserved_size = reserved_pages * page_size;
        if reserved_size >= flash_total {
            return Err(format!(
                "flash {} меньше резерва под bootloader ({} при секторе {})",
                size(flash_total),
                size(reserved_size),
                size(page_size)
            ));
        }
        let Some(local_erase) = erase_size_at(reserved_size - 1).filter(|erase| *erase > 0) else {
            return Err("не удалось определить размер сектора на границе резерва".to_string());
        };
        if !reserved_size.is_multiple_of(local_erase) {
            // На реальных чипах ST не встречается (erase_size регионов чипа
            // кратны друг другу), но битый линкер-скрипт хуже честного отказа.
            return Err(format!(
                "резерв {} не ложится на границу сектора {}",
                size(reserved_size),
                size(local_erase)
            ));
        }

        let remaining_pages = (flash_total - reserved_size) / page_size;
        if remaining_pages < 3 {
            // Дальше растить резерв бессмысленно — остаток только уменьшится.
            return Err(format!(
                "flash {} при секторе {}: под BOOTLOADER+STATE уходит {}, \
                 на ACTIVE+DFU остаётся стираемых страниц: {remaining_pages} (нужно 3)",
                size(flash_total),
                size(page_size),
                size(reserved_size)
            ));
        }
        // Пополам остаток не делится: DFU обязан быть на страницу больше
        // ACTIVE (проверка 3).
        let active_pages = (remaining_pages - 1) / 2;
        let dfu_pages = remaining_pages - active_pages;

        // Проверка 4, решённая относительно STATE: сколько журнала нужно под
        // такой ACTIVE и в сколько локальных секторов это укладывается.
        let state_needed = (2 + 4 * active_pages) * write_size;
        let state_length = state_needed.div_ceil(local_erase) * local_erase;
        let bootloader_length = reserved_size.saturating_sub(state_length);
        let state_offset = reserved_size - state_length.min(reserved_size);
        if bootloader_length < BOOTLOADER_MIN || erase_size_at(state_offset) != Some(local_erase) {
            // Либо журнал съел место под сам бинарник, либо начало STATE
            // попало в зону с другим размером сектора — в обоих случаях
            // помогает более крупный резерв: он уменьшает ACTIVE, а с ним и
            // журнал.
            reserved_pages += 1;
            continue;
        }

        let active_origin = FLASH_BASE + reserved_size;
        let active_length = active_pages * page_size;
        let partitions = OtaPartitions {
            bootloader_length,
            bootloader_state_origin: FLASH_BASE + state_offset,
            bootloader_state_length: state_length,
            active_origin,
            active_length,
            dfu_origin: active_origin + active_length,
            dfu_length: dfu_pages * page_size,
        };
        assert_embassy_boot_invariants(&partitions, page_size, write_size);
        return Ok(partitions);
    }
}

/// Те же четыре проверки, что `assert_partitions` делает в рантайме на плате —
/// но здесь, на этапе генерации. Паника вместо `Err`: `compute_ota_partitions`
/// обязан выдавать только валидные раскладки, невалидная — баг в нём самом, а
/// не «чип не подошёл». Ровно этой проверки не хватало: 279 из 972 прежних
/// раскладок нарушали четвёртую и роняли bootloader на первом же старте.
fn assert_embassy_boot_invariants(p: &OtaPartitions, page_size: u64, write_size: u64) {
    assert!(
        p.active_length.is_multiple_of(page_size),
        "ACTIVE {} не кратен PAGE_SIZE {page_size}",
        p.active_length
    );
    assert!(
        p.dfu_length.is_multiple_of(page_size),
        "DFU {} не кратен PAGE_SIZE {page_size}",
        p.dfu_length
    );
    assert!(
        p.dfu_length >= p.active_length + page_size,
        "DFU {} не на страницу больше ACTIVE {}",
        p.dfu_length,
        p.active_length
    );
    let journal_words = 2 + 4 * (p.active_length / page_size);
    let state_words = p.bootloader_state_length / write_size;
    assert!(
        journal_words <= state_words,
        "журналу прогресса нужно {journal_words} слов, в BOOTLOADER_STATE помещается {state_words}"
    );
    assert!(
        p.active_origin == p.bootloader_state_origin + p.bootloader_state_length
            && p.dfu_origin == p.active_origin + p.active_length,
        "партиции не стыкуются встык"
    );
}

fn format_addr(v: u64) -> String {
    format!("0x{v:08X}")
}

/// `K`-нотация линкер-скрипта, когда значение делится ровно — а оно всегда
/// делится: все размеры в `MemoryLayout` строятся из `erase_size`/RAM
/// регионов чипов ST, которые сами всегда кратны 1024. Запасной путь на
/// байты — просто чтобы не запаниковать, если это когда-нибудь окажется не
/// так, а не потому, что такой чип реально существует.
fn format_size(v: u64) -> String {
    if v.is_multiple_of(1024) {
        format!("{}K", v / 1024)
    } else {
        v.to_string()
    }
}

/// Одно поле Rhai-мапы. Экранирования в Rhai-строках здесь нет: все значения
/// — либо числа/адреса, либо тексты причин из `compute_ota_partitions`, где
/// кавычек нет по построению (проверяется `assert`, чтобы это не разъехалось
/// молча при правке сообщений).
fn push_field(out: &mut String, key: &str, value: &str) {
    assert!(!value.contains('"'), "кавычка в значении {key}: {value}");
    out.push_str(&format!("        \"{key}\": \"{value}\",\n"));
}

fn format_memory_layouts(layouts: &BTreeMap<&str, MemoryLayout>) -> String {
    let with_ota = layouts.values().filter(|m| m.ota.is_ok()).count();
    let mut out = String::new();
    out.push_str(MEMORY_BEGIN);
    out.push_str(&format!(
        " ({} шт., из них {with_ota} с OTA, cargo run --manifest-path \
         chip-data-gen/Cargo.toml)\n",
        layouts.len(),
    ));
    out.push_str("const MEMORY_LAYOUT = #{\n");
    for (suffix, m) in layouts {
        out.push_str(&format!("    \"{suffix}\": #{{\n"));
        push_field(&mut out, "flash_origin", &format_addr(m.flash_origin));
        push_field(&mut out, "flash_length", &format_size(m.flash_length));
        push_field(&mut out, "ram_origin", &format_addr(m.ram_origin));
        push_field(&mut out, "ram_length", &format_size(m.ram_length));
        if let Some((origin, length)) = m.persist {
            push_field(&mut out, "persist_origin", &format_addr(origin));
            push_field(&mut out, "persist_length", &format_size(length));
        }
        if let Some((origin, length)) = m.panic {
            push_field(&mut out, "panic_origin", &format_addr(origin));
            push_field(&mut out, "panic_length", &format_size(length));
        }
        if !m.extra_regions.is_empty() {
            out.push_str("        \"extra_regions\": [\n");
            for line in &m.extra_regions {
                assert!(!line.contains('"'), "кавычка в строке региона: {line}");
                out.push_str(&format!("            \"{line}\",\n"));
            }
            out.push_str("        ],\n");
        }
        push_field(&mut out, "write_size", &m.write_size.to_string());
        match &m.ota {
            Ok(p) => {
                push_field(&mut out, "ota", "true");
                push_field(
                    &mut out,
                    "bootloader_length",
                    &format_size(p.bootloader_length),
                );
                push_field(
                    &mut out,
                    "bootloader_state_origin",
                    &format_addr(p.bootloader_state_origin),
                );
                push_field(
                    &mut out,
                    "bootloader_state_length",
                    &format_size(p.bootloader_state_length),
                );
                push_field(&mut out, "active_origin", &format_addr(p.active_origin));
                push_field(&mut out, "active_length", &format_size(p.active_length));
                push_field(&mut out, "dfu_origin", &format_addr(p.dfu_origin));
                push_field(&mut out, "dfu_length", &format_size(p.dfu_length));
            }
            Err(note) => {
                push_field(&mut out, "ota", "false");
                push_field(&mut out, "note", note);
            }
        }
        out.push_str("    },\n");
    }
    out.push_str("};\n");
    out
}

/// Только чипы, у которых `embassy-stm32` требует выбора банковой схемы
/// (несколько карт памяти) — для остальных фичи нет вовсе, и хук подставляет
/// пустую строку.
fn format_bank_modes(modes: &BTreeMap<&str, &'static str>) -> String {
    let mut out = String::new();
    out.push_str(BANKS_BEGIN);
    out.push_str(&format!(
        " ({} шт., cargo run --manifest-path chip-data-gen/Cargo.toml)\n",
        modes.len()
    ));
    out.push_str("const BANK_MODE = #{\n");
    for (suffix, mode) in modes {
        out.push_str(&format!("    \"{suffix}\": \"{mode}\",\n"));
    }
    out.push_str("};\n");
    out
}

/// Множество идентификаторов чипов, которые знает локально установленный
/// `probe-rs` (`probe-rs chip list`), например `STM32F407VE` — без суффикса
/// корпус/темп.диапазон (`Tx`/`Hx`/...), см. обоснование в CLAUDE.md.
/// Версия `embassy-stm32` так, как она объявлена в `cross/Cargo.toml` — не
/// разрешённая cargo: сверять надо именно объявление, иначе безобидный
/// патч-релиз в реестре ронял бы тест штампа.
fn declared_embassy_version(repo_root: &Path) -> anyhow::Result<String> {
    let manifest_path = repo_root.join("cross").join("Cargo.toml");
    let manifest = fs::read_to_string(&manifest_path)
        .with_context(|| format!("не удалось прочитать {}", manifest_path.display()))?;
    parse_declared_embassy_version(&manifest).with_context(|| {
        format!(
            "в {} не нашлась версия embassy-stm32",
            manifest_path.display()
        )
    })
}

fn parse_declared_embassy_version(manifest: &str) -> Option<String> {
    let line = manifest
        .lines()
        .find(|line| line.trim_start().starts_with("embassy-stm32"))?;
    let (_, rest) = line.split_once("version")?;
    let rest = rest.trim_start().strip_prefix('=')?;
    let rest = rest.trim_start().strip_prefix('"')?;
    let (version, _) = rest.split_once('"')?;
    Some(version.to_owned())
}

fn probe_rs_version() -> anyhow::Result<String> {
    let output = Command::new("probe-rs")
        .arg("--version")
        .output()
        .context("не удалось запустить `probe-rs --version`")?;
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first = stdout.lines().next().unwrap_or_default().trim();
    // "probe-rs 0.31.0 (git commit: ...)" -> "0.31.0"
    Ok(first
        .strip_prefix("probe-rs ")
        .and_then(|rest| rest.split_whitespace().next())
        .unwrap_or(first)
        .to_owned())
}

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

/// Строка-штамп с версиями источников данных. Её сверяет тест
/// `generated_blocks_match_the_declared_embassy_version`: без штампа связь
/// «подняли embassy-stm32 — перегенерируйте списки» держалась только на памяти
/// мейнтейнера, а поднимает версию обычно бот, молча.
pub const SOURCE_STAMP_PREFIX: &str = "// Источник: embassy-stm32 ";

fn format_source_stamp(embassy_version: &str, probe_rs_version: &str) -> String {
    format!(
        "{SOURCE_STAMP_PREFIX}{embassy_version} (cross/Cargo.toml), probe-rs {probe_rs_version}\n"
    )
}

fn format_chip_list(suffixes: &[&str], stamp: &str) -> String {
    let mut out = String::new();
    out.push_str(CHIPS_BEGIN);
    out.push_str(" (");
    out.push_str(&suffixes.len().to_string());
    out.push_str(" шт., cargo run --manifest-path chip-data-gen/Cargo.toml)\n");
    out.push_str(stamp);
    out.push_str("const CHIPS = [\n");
    for suffix in suffixes {
        out.push_str("    \"");
        out.push_str(suffix);
        out.push_str("\",\n");
    }
    out.push_str("];\n");
    out
}

fn format_package_choices(choices: &BTreeMap<&str, Vec<String>>) -> String {
    let mut out = String::new();
    out.push_str(PACKAGES_BEGIN);
    out.push_str(" (");
    out.push_str(&choices.len().to_string());
    out.push_str(" шт., cargo run --manifest-path chip-data-gen/Cargo.toml)\n");
    out.push_str("const PACKAGE_CHOICES = #{\n");
    for (suffix, candidates) in choices {
        out.push_str("    \"");
        out.push_str(suffix);
        out.push_str("\": [");
        for (i, candidate) in candidates.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push('"');
            out.push_str(candidate);
            out.push('"');
        }
        out.push_str("],\n");
    }
    out.push_str("};\n");
    out
}

/// Заменяет содержимое между `begin_marker` и `end_marker` (маркеры входят в
/// заменяемый блок) на `generated`. Используется для обоих сгенерированных
/// блоков `chip-select.rhai` — списка чипов и таблицы уточнения корпусировки.
fn write_generated_block(
    rhai_path: &Path,
    begin_marker: &str,
    end_marker: &str,
    generated: &str,
) -> anyhow::Result<()> {
    let original = fs::read_to_string(rhai_path)
        .with_context(|| format!("не удалось прочитать {}", rhai_path.display()))?;

    let begin = original
        .find(begin_marker)
        .with_context(|| format!("{begin_marker} не найден в {}", rhai_path.display()))?;
    let end = original
        .find(end_marker)
        .with_context(|| format!("{end_marker} не найден в {}", rhai_path.display()))?;
    if end < begin {
        bail!(
            "{end_marker} стоит раньше {begin_marker} в {}",
            rhai_path.display()
        );
    }

    let mut updated = String::with_capacity(original.len() + generated.len());
    updated.push_str(&original[..begin]);
    updated.push_str(generated);
    updated.push_str(&original[end..]);

    fs::write(rhai_path, updated)
        .with_context(|| format!("не удалось записать {}", rhai_path.display()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Одноконфигурационный чип: `stm32-data` печатает `memory: &[&[` одной
    /// строкой.
    const SINGLE_CONFIG: &str = r#"
pub static METADATA: Metadata = Metadata {
    name: "STM32L476RG",
    memory: &[&[
        MemoryRegion {
            name: "BANK_1",
            kind: MemoryRegionKind::Flash,
            address: 0x8000000,
            size: 1048576,
            settings: Some(FlashSettings { erase_size: 2048, write_size: 8, erase_value: 255 }),
        },
        MemoryRegion {
            name: "SRAM",
            kind: MemoryRegionKind::Ram,
            address: 0x20000000,
            size: 98304,
            settings: None,
        },
    ]],
    peripherals: &[],
};
"#;

    /// Многоконфигурационный: `memory: &[` и `&[` на следующей строке. Ровно
    /// на этом варианте прежний парсер молча падал, и ~200 чипов исчезали.
    const MULTI_CONFIG: &str = r#"
pub static METADATA: Metadata = Metadata {
    name: "STM32G474RE",
    memory: &[
        &[
            MemoryRegion {
                name: "BANK_1",
                kind: MemoryRegionKind::Flash,
                address: 0x8000000,
                size: 524288,
                settings: Some(FlashSettings { erase_size: 4096, write_size: 8, erase_value: 255 }),
            },
            MemoryRegion {
                name: "SRAM1",
                kind: MemoryRegionKind::Ram,
                address: 0x20000000,
                size: 81920,
                settings: None,
            },
        ],
        &[
            MemoryRegion {
                name: "BANK_1",
                kind: MemoryRegionKind::Flash,
                address: 0x8000000,
                size: 262144,
                settings: Some(FlashSettings { erase_size: 2048, write_size: 8, erase_value: 255 }),
            },
            MemoryRegion {
                name: "BANK_2",
                kind: MemoryRegionKind::Flash,
                address: 0x8040000,
                size: 262144,
                settings: Some(FlashSettings { erase_size: 2048, write_size: 8, erase_value: 255 }),
            },
        ],
    ],
    peripherals: &[],
};
"#;

    fn configs(text: &str) -> Vec<Vec<RawRegion>> {
        split_memory_configs(text)
            .expect("карта памяти должна найтись")
            .into_iter()
            .map(|section| parse_regions(section).expect("регионы должны разобраться"))
            .collect()
    }

    #[test]
    fn parses_both_metadata_formats() {
        let single = configs(SINGLE_CONFIG);
        assert_eq!(single.len(), 1);
        assert_eq!(single[0].len(), 2);

        let multi = configs(MULTI_CONFIG);
        assert_eq!(multi.len(), 2);
        assert_eq!(multi[0][0].size, 524288);
        assert_eq!(multi[1].len(), 2);
        assert_eq!(multi[1][1].name, "BANK_2");
    }

    #[test]
    fn bank_mode_is_empty_only_for_single_config_chips() {
        let (regions, mode) = select_memory_config(configs(SINGLE_CONFIG)).expect("одна карта");
        assert_eq!(mode, "");
        assert_eq!(regions.len(), 2);

        // Из двух карт выбирается одобанковая — та, где есть BANK_1 и нет
        // BANK_2 (предикат повторяет build.rs embassy-stm32).
        let (regions, mode) = select_memory_config(configs(MULTI_CONFIG)).expect("две карты");
        assert_eq!(mode, "single-bank");
        assert_eq!(regions[0].size, 524288);
    }

    /// Один flash-регион, `size`/`erase_size` в байтах.
    fn uniform_flash(size: u64, erase_size: u64, write_size: u64) -> Vec<RawRegion> {
        vec![
            RawRegion {
                name: "BANK_1".to_string(),
                kind: RegionKind::Flash,
                address: FLASH_BASE,
                size,
                erase_write_size: Some((erase_size, write_size)),
            },
            RawRegion {
                name: "SRAM".to_string(),
                kind: RegionKind::Ram,
                address: RAM_BASE,
                size: 64 * 1024,
                erase_write_size: None,
            },
        ]
    }

    #[test]
    fn state_grows_with_active_to_fit_the_swap_journal() {
        // L476RG: 1 MiB при секторе 2 KiB. Одного сектора под STATE не хватает
        // (журналу нужно (2 + 4*247) слов по 8 байт) — раньше здесь стоял
        // ровно один сектор, и bootloader падал в панику на старте.
        let layout = compute_memory_layout(&uniform_flash(1024 * 1024, 2048, 8))
            .expect("раскладка должна посчитаться");
        let ota = layout.ota.expect("OTA должна помещаться");
        assert!(
            ota.bootloader_state_length > 2048,
            "STATE {} — одного сектора мало",
            ota.bootloader_state_length
        );
        // Сам инвариант проверяет assert_embassy_boot_invariants внутри
        // compute_ota_partitions; здесь фиксируем, что он не обошёлся малым.
        assert_eq!(ota.bootloader_state_length, 8192);
    }

    #[test]
    fn chip_with_too_few_sectors_reports_why() {
        // H723VE: 512 KiB одним регионом с сектором 128 KiB — 4 сектора, а
        // схеме нужно минимум 5 (BOOTLOADER + STATE + ACTIVE + 2×DFU).
        let layout = compute_memory_layout(&uniform_flash(512 * 1024, 128 * 1024, 32))
            .expect("раскладка должна посчитаться");
        let note = layout.ota.expect_err("OTA не должна помещаться");
        assert!(note.contains("512 KiB"), "{note}");
        assert!(note.contains("нужно 3"), "{note}");
        // Но FLASH/RAM всё равно посчитаны — memory.x не остаётся заглушкой.
        assert_eq!(layout.flash_length, 512 * 1024);
        assert!(layout.persist.is_some());
        assert!(layout.panic.is_some(), "дамп паники живёт рядом с PERSIST");
    }

    #[test]
    fn liquid_conditionals_are_stripped_from_manifests() {
        let manifest =
            "members = [\"app\", {% if ota == \"true\" %}\"boot\", {% endif %}\"bsp\"]\n";
        assert_eq!(strip_liquid(manifest), "members = [\"app\", \"bsp\"]\n");
        // Без условий текст не меняется.
        let plain = "members = [\"app\"]\n";
        assert_eq!(strip_liquid(plain), plain);
    }
}
