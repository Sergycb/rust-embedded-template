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

    let memory_layouts: BTreeMap<&str, MemoryLayout> = suffixes
        .iter()
        .filter_map(|suffix| {
            let layout = parse_chip_memory(&cargo_metadata.stm32_metapac_chips_dir, suffix)
                .ok()
                .and_then(|regions| compute_memory_layout(&regions));
            layout.map(|layout| (*suffix, layout))
        })
        .collect();

    println!(
        "embassy-stm32: {} чип-фич; probe-rs: {} целей; итоговый список: {} (отброшено {}, \
         нет цели probe-rs); более точная цель probe-rs, чем базовая, найдена для {} чипов; \
         memory.x посчитан для {} из {} чипов",
        cargo_metadata.embassy_chip_features.len(),
        probe_rs_chips.len(),
        suffixes.len(),
        dropped,
        package_choices.len(),
        memory_layouts.len(),
        suffixes.len(),
    );

    let rhai_path = repo_root.join("chip-select.rhai");
    write_generated_block(
        &rhai_path,
        CHIPS_BEGIN,
        CHIPS_END,
        &format_chip_list(&suffixes),
    )?;
    write_generated_block(
        &rhai_path,
        PACKAGES_BEGIN,
        PACKAGES_END,
        &format_package_choices(&package_choices),
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
/// для `memory.x`: адрес, размер и (для flash) реальный размер стирания и
/// минимальную порцию записи.
struct RawRegion {
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
    Other,
}

/// Разбирает `<stm32_metapac_chips_dir>/<chip>/metadata.rs` — это обычный
/// Rust-файл с литералом `pub static METADATA: Metadata = Metadata { ... }`,
/// сгенerированный из `stm32-data`, не то, что компилируется под конкретную
/// фичу: можно читать текстом для любого чипа без сборки. Берёт только
/// первый вариант карты памяти (`memory: &[&[ ... ]]`) — у чипов с
/// несколькими вариантами (dual-bank/single-bank) это то же, что получает
/// `embassy-stm32` по умолчанию (см. фичи `dual-bank`/`single-bank` в его
/// `Cargo.toml`, шаблон их не трогает).
fn parse_chip_memory(chips_dir: &Path, suffix: &str) -> anyhow::Result<Vec<RawRegion>> {
    // stm32-metapac каталоги — с полным именем чипа ("stm32f407ve"), а не
    // просто суффиксом ("f407ve") без префикса, как в CHIPS/PACKAGE_CHOICES.
    let path = chips_dir.join(format!("stm32{suffix}")).join("metadata.rs");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("не удалось прочитать {}", path.display()))?;

    let list_start = text
        .find("memory: &[&[")
        .context("memory: &[&[ не найден")?;
    let after = &text[list_start + "memory: &[&[".len()..];
    let list_end = after
        .find("]]")
        .context("закрывающий ]] у memory не найден")?;
    let section = &after[..list_end];

    let regions = section
        .split("MemoryRegion {")
        .skip(1)
        .map(|block| {
            let kind = match field(block, "kind").context("MemoryRegion без kind")? {
                s if s.ends_with("Flash") => RegionKind::Flash,
                s if s.ends_with("Ram") => RegionKind::Ram,
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
                kind,
                address,
                size,
                erase_write_size,
            })
        })
        .collect::<anyhow::Result<Vec<_>>>()
        .with_context(|| format!("не удалось разобрать {}", path.display()))?;
    if regions.is_empty() {
        bail!("{}: не найдено ни одного MemoryRegion", path.display());
    }
    Ok(regions)
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

/// Готовые для `memory.x` адреса/размеры одного чипа. Всё в байтах —
/// `format_memory_layouts` переводит в hex/`K`-нотацию для Rhai.
struct MemoryLayout {
    flash_origin: u64,
    flash_length: u64,
    bootloader_state_origin: u64,
    bootloader_state_length: u64,
    active_origin: u64,
    active_length: u64,
    dfu_origin: u64,
    dfu_length: u64,
    ram_origin: u64,
    ram_length: u64,
    persist_origin: u64,
    persist_length: u64,
    /// Он же `PAGE_SIZE`/`BUFFER_SIZE` в терминах `embassy-boot` — const
    /// generic параметр `BootLoader::prepare::<_, _, _, N>` (буфер подкачки
    /// для алгоритма swap, размер стирания, а не минимальная порция записи
    /// `NorFlash::WRITE_SIZE`, несмотря на имя плейсхолдера `write_size` в
    /// шаблоне — переименовывать не стали ради обратной совместимости с уже
    /// сгенерированными проектами, которые задают его через `--define`).
    write_size: u64,
}

const FLASH_BASE: u64 = 0x0800_0000;
const RAM_BASE: u64 = 0x2000_0000;
/// Ориентир на размер бинарника bootloader (embassy-boot + defmt +
/// cortex-m-rt) — на практике укладывается в 20-30 KiB, отсюда небольшой
/// запас. Bootloader сам по себе не стирается во время работы, поэтому ему
/// не нужно быть кратным PAGE_SIZE — только BOOTLOADER_STATE после него.
const BOOTLOADER_TARGET: u64 = 32 * 1024;
/// Чипы с совсем маленьким RAM (несколько KiB) не проходят автогенерацию —
/// смысла отводить под PERSIST фиксированный кусок нет, оставляем плейсхолдер
/// на ручное заполнение, как раньше.
const MIN_RAM_FOR_AUTO: u64 = 8 * 1024;
const PERSIST_LENGTH: u64 = 1024;

/// Вычисляет разбиение flash/RAM под bootloader (`BOOTLOADER_STATE`/`ACTIVE`/
/// `DFU`, см. `cross/boot/src/main.rs`) и RAM (`PERSIST`) по реальным
/// границам секторов чипа. `None`, если авточипу не подходит (например,
/// слишком маленький flash/RAM) — тогда `chip-select.rhai` оставляет
/// `memory.x` как плейсхолдер, ничего не перезаписывая.
///
/// Ключевое ограничение (реальный runtime-assert в `embassy-boot`,
/// `assert_partitions` в `boot_loader.rs`): `ACTIVE`/`DFU` обязаны быть
/// кратны `PAGE_SIZE` — максимальному `erase_size` среди ВСЕХ flash-регионов
/// чипа, а не только того региона, где физически лежит партиция. Это так,
/// потому что `cross/boot/src/main.rs` использует один и тот же generic
/// `Flash` (весь чип целиком, см. коммит про `BANK1_REGION`) для
/// active/dfu/state сразу — у него один `NorFlash::ERASE_SIZE` на всех.
/// `BOOTLOADER`/`BOOTLOADER_STATE` этому ограничению не подчиняются (нет
/// прямого сравнения с `PAGE_SIZE` в `assert_partitions`), поэтому подбираются
/// по локальной границе сектора — иначе на чипах с крупными секторами
/// (F4/F7/H7, до 128 KiB) один только отступ под bootloader съедал бы
/// четверть чипа.
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
    let page_size = chain
        .iter()
        .filter_map(|r| r.erase_write_size)
        .map(|(erase, _)| erase)
        .max()?;
    let write_size = chain[0].erase_write_size?.1;
    if page_size == 0 || write_size == 0 {
        return None;
    }

    let erase_size_at = |offset_from_flash_base: u64| -> Option<u64> {
        let addr = FLASH_BASE + offset_from_flash_base;
        chain
            .iter()
            .find(|r| addr >= r.address && addr < r.address + r.size)
            .and_then(|r| r.erase_write_size)
            .map(|(erase, _)| erase)
    };

    // Растим reserved_pages, пока в зарезервированном блоке не останется
    // места под сам бинарник bootloader'а (не только BOOTLOADER_STATE): на
    // чипах, где самый первый регион уже имеет крупный erase_size (H7 —
    // 128 KiB с адреса 0, без мелких секторов в начале, в отличие от F4),
    // один PAGE_SIZE целиком уходит под BOOTLOADER_STATE, и bootloader
    // размещать некуда.
    let mut reserved_pages = BOOTLOADER_TARGET.div_ceil(page_size).max(1);
    let (reserved_size, bootloader_state_length) = loop {
        let reserved_size = reserved_pages * page_size;
        if reserved_size >= flash_total {
            return None;
        }
        let local_erase = erase_size_at(reserved_size - 1)?;
        if local_erase == 0 || reserved_size % local_erase != 0 {
            // Крупный (кратный PAGE_SIZE) отступ должен приходиться ровно на
            // границу локального сектора — на реальных чипах ST это всегда
            // так (erase_size регионов чипа кратны друг другу), но если
            // вдруг нет — безопаснее пропустить чип, чем сгенерировать
            // битый memory.x.
            return None;
        }
        if local_erase < reserved_size {
            break (reserved_size, local_erase);
        }
        // Целиком один сектор — под bootloader нет места, берём ещё одну
        // страницу. Ограничение на число попыток — не зависнуть на чипе с
        // патологически большими секторами.
        reserved_pages += 1;
        if reserved_pages > 8 {
            return None;
        }
    };
    let bootloader_state_origin = FLASH_BASE + reserved_size - bootloader_state_length;

    let remaining_pages = (flash_total - reserved_size) / page_size;
    if remaining_pages < 3 {
        // Меньше нет смысла: минимум 1 страница ACTIVE + запас DFU >= ACTIVE
        // + 1 страница, см. doc-комментарий структуры.
        return None;
    }
    let active_pages = (remaining_pages - 1) / 2;
    let dfu_pages = remaining_pages - active_pages;
    let active_length = active_pages * page_size;
    let dfu_length = dfu_pages * page_size;
    let active_origin = FLASH_BASE + reserved_size;
    let dfu_origin = active_origin + active_length;

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
    if ram_total < MIN_RAM_FOR_AUTO {
        return None;
    }
    let ram_origin = RAM_BASE;
    let ram_length = ram_total - PERSIST_LENGTH;
    let persist_origin = RAM_BASE + ram_length;

    Some(MemoryLayout {
        flash_origin: FLASH_BASE,
        flash_length: flash_total,
        bootloader_state_origin,
        bootloader_state_length,
        active_origin,
        active_length,
        dfu_origin,
        dfu_length,
        ram_origin,
        ram_length,
        persist_origin,
        persist_length: PERSIST_LENGTH,
        write_size,
    })
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

fn format_memory_layouts(layouts: &BTreeMap<&str, MemoryLayout>) -> String {
    let mut out = String::new();
    out.push_str(MEMORY_BEGIN);
    out.push_str(" (");
    out.push_str(&layouts.len().to_string());
    out.push_str(" шт., cargo run --manifest-path chip-data-gen/Cargo.toml)\n");
    out.push_str("const MEMORY_LAYOUT = #{\n");
    for (suffix, m) in layouts {
        out.push_str("    \"");
        out.push_str(suffix);
        out.push_str("\": #{\n");
        out.push_str(&format!(
            "        \"flash_origin\": \"{}\",\n",
            format_addr(m.flash_origin)
        ));
        out.push_str(&format!(
            "        \"flash_length\": \"{}\",\n",
            format_size(m.flash_length)
        ));
        out.push_str(&format!(
            "        \"bootloader_state_origin\": \"{}\",\n",
            format_addr(m.bootloader_state_origin)
        ));
        out.push_str(&format!(
            "        \"bootloader_state_length\": \"{}\",\n",
            format_size(m.bootloader_state_length)
        ));
        out.push_str(&format!(
            "        \"active_origin\": \"{}\",\n",
            format_addr(m.active_origin)
        ));
        out.push_str(&format!(
            "        \"active_length\": \"{}\",\n",
            format_size(m.active_length)
        ));
        out.push_str(&format!(
            "        \"dfu_origin\": \"{}\",\n",
            format_addr(m.dfu_origin)
        ));
        out.push_str(&format!(
            "        \"dfu_length\": \"{}\",\n",
            format_size(m.dfu_length)
        ));
        out.push_str(&format!(
            "        \"ram_origin\": \"{}\",\n",
            format_addr(m.ram_origin)
        ));
        out.push_str(&format!(
            "        \"ram_length\": \"{}\",\n",
            format_size(m.ram_length)
        ));
        out.push_str(&format!(
            "        \"persist_origin\": \"{}\",\n",
            format_addr(m.persist_origin)
        ));
        out.push_str(&format!(
            "        \"persist_length\": \"{}\",\n",
            format_size(m.persist_length)
        ));
        out.push_str(&format!("        \"write_size\": \"{}\",\n", m.write_size));
        out.push_str("    },\n");
    }
    out.push_str("};\n");
    out
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

fn format_chip_list(suffixes: &[&str]) -> String {
    let mut out = String::new();
    out.push_str(CHIPS_BEGIN);
    out.push_str(" (");
    out.push_str(&suffixes.len().to_string());
    out.push_str(" шт., cargo run --manifest-path chip-data-gen/Cargo.toml)\n");
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
