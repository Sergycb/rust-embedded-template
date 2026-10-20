use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

/// Каталог с таблицами, которые `chip-select.rhai` подключает через `import`.
/// Пишется целиком при каждом прогоне — руками там ничего не правится.
const DATA_DIR: &str = "chip-data";
/// Первая строка каждого файла в [`DATA_DIR`].
const DATA_HEADER: &str = "// Сгенерировано chip-data-gen, руками не править: cargo run --manifest-path chip-data-gen/Cargo.toml\n";
/// Семейства, у которых двухбанковый режим стоит с завода, — по версии блока
/// FLASH в метаданных, с именем варианта регистров option bytes в
/// `crates-cross/bsp/src/banks.rs`. Заводское значение — знание документации, а
/// не метаданных: `DBANK`/`DUAL_BANK` = 1 в FLASH_OPTR нового кристалла (RM0440
/// G4, RM0444 G0, RM0432 L4+, RM0438 L5; OpenOCD `stm32l4x.c`). У F4 с 1 MiB
/// (`DB1M` = 0) и F76x/F77x (`nDBANK` = 1) с завода одобанковый, и их здесь нет.
const FACTORY_DUAL_BANK: &[(&str, &str)] =
    &[("g0x1", "g0"), ("g4c3", "g4"), ("l4", "l4"), ("l5", "l5")];
/// Имя блока независимого сторожевого таймера у подавляющего большинства
/// чипов (800 из 891 наборов метаданных). Таблица `WATCHDOG` перечисляет
/// только исключения, а хук подставляет это значение для всех остальных — по
/// той же схеме, что и `BANK_MODE`.
const DEFAULT_WATCHDOG: &str = "IWDG";
/// Любая реальная чип-фича годится — нужна только чтобы `cargo metadata` смог
/// разрешить зависимость `embassy-stm32`: в самом `crates-cross/Cargo.toml` вместо
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
    let mut watchdogs: BTreeMap<&str, String> = BTreeMap::new();
    let mut cpu_freq_boost: Vec<&str> = Vec::new();
    let mut interfaces: BTreeMap<&str, Vec<&'static str>> = BTreeMap::new();
    let mut instances: BTreeMap<&str, BTreeMap<&'static str, Vec<String>>> = BTreeMap::new();
    let mut rngs: BTreeMap<&str, (String, RngClock)> = BTreeMap::new();
    let mut caches: BTreeMap<&str, Vec<&'static str>> = BTreeMap::new();
    let mut switch_variants: BTreeMap<&str, &'static str> = BTreeMap::new();
    let mut shared_metadata_cache: BTreeMap<PathBuf, SharedMetadata> = BTreeMap::new();
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
        if let Some(layout) = compute_memory_layout(suffix, &regions) {
            memory_layouts.insert(suffix, layout);
        }
        let (shared_path, shared) = shared_metadata(
            &cargo_metadata.stm32_metapac_chips_dir,
            suffix,
            &mut shared_metadata_cache,
        )
        .with_context(|| format!("чип {suffix}"))?;
        let watchdog =
            chip_watchdog(suffix, shared, &shared_path).with_context(|| format!("чип {suffix}"))?;
        if watchdog != DEFAULT_WATCHDOG {
            watchdogs.insert(suffix, watchdog);
        }
        if shared.cpu_freq_boost {
            cpu_freq_boost.push(suffix);
        }
        let cache = chip_caches(&cargo_metadata.stm32_metapac_chips_dir, shared)
            .with_context(|| format!("чип {suffix}"))?;
        if !cache.is_empty() {
            caches.insert(suffix, cache);
        }
        interfaces.insert(suffix, shared.interfaces.clone());
        instances.insert(suffix, shared.instances.clone());
        // HSI48 `embassy-stm32` включает только при CRS (`#[cfg(crs)]` в
        // `rcc`): без него источник «HSI48» — клок, который не идёт
        // (STM32WB10/15/30/50).
        let clock = rng_clock(suffix)
            .filter(|&(kind, source)| shared.has_crs || (kind != "default" && source != "HSI48"));
        if let (Some(interrupt), Some(clock)) = (&shared.rng_interrupt, clock) {
            rngs.insert(suffix, (interrupt.clone(), clock));
        }
        // Одобанковая раскладка на чипе, который с завода двухбанковый: режим
        // переключает прошивка (`bsp::banks`).
        if bank_mode == "single-bank"
            && let Some(variant) = shared.flash_version.as_deref().and_then(|version| {
                FACTORY_DUAL_BANK
                    .iter()
                    .find(|(flash, _)| *flash == version)
                    .map(|(_, variant)| *variant)
            })
        {
            switch_variants.insert(*suffix, variant);
        }
    }
    let bank_switch = bank_switch_bits(&switch_variants, &memory_layouts);
    let with_ota = memory_layouts.values().filter(|m| m.ota.is_ok()).count();
    let with_config = memory_layouts
        .values()
        .filter(|m| m.config.is_some())
        .count();
    let with_external = memory_layouts
        .values()
        .filter(|m| m.external.is_ok())
        .count();
    let config_keeps_ota = memory_layouts
        .values()
        .filter(|m| m.config.as_ref().is_some_and(|config| config.ota.is_ok()))
        .count();

    // Инвариант «раскладка посчитана для каждого чипа», который до сих пор жил
    // только в прозе MAINTAINING.md. Без него смена формата в `stm32-data`
    // (неизвестный `kind`, потерянная непрерывная цепочка flash) тихо оставила
    // бы нужное число чипов с плейсхолдером `memory.x`, и единственным следом
    // осталась бы строка сводки ниже — цифра, на которую никто не смотрит.
    // Считаем до печати: упавший прогон не должен успеть переписать
    // `chip-data/`.
    let without_layout: Vec<&str> = suffixes
        .iter()
        .filter(|suffix| !memory_layouts.contains_key(*suffix))
        .copied()
        .collect();
    anyhow::ensure!(
        without_layout.is_empty(),
        "раскладка памяти не посчитана для {} чипов из {}: {}. Их `memory.x` остался бы \
         плейсхолдером, и проект собрался бы только после ручного заполнения. Причина, скорее \
         всего, — смена формата метаданных в stm32-data: посмотрите на первый из этих чипов \
         ({}/stm32{}/metadata.rs) и на `compute_memory_layout`",
        without_layout.len(),
        suffixes.len(),
        without_layout
            .iter()
            .take(10)
            .copied()
            .collect::<Vec<_>>()
            .join(", "),
        cargo_metadata.stm32_metapac_chips_dir.display(),
        without_layout[0],
    );

    println!(
        "embassy-stm32: {} чип-фич; probe-rs: {} целей; итоговый список: {} (отброшено {}, \
         нет цели probe-rs); более точная цель probe-rs, чем базовая, найдена для {} чипов; \
         выбор банковой схемы нужен {} чипам; сторож зовётся не {} у {} чипов; option byte \
         CPU_FREQ_BOOST есть у {} чипов; кэш включается у {} чипов (без кэша ядра M7); RNG подключается у {} чипов; режим банков переключает прошивка у {} чипов; memory.x \
         посчитан для {} из {} чипов, из них \
         с OTA {} (остальные — один образ на весь flash, без crates-cross/boot); раздел настроек \
         возможен на {} чипах, из них с сохранением OTA — {}; OTA с DFU на внешней флеши \
         помещается на {} чипах",
        cargo_metadata.embassy_chip_features.len(),
        probe_rs_chips.len(),
        suffixes.len(),
        dropped,
        package_choices.len(),
        bank_modes.len(),
        DEFAULT_WATCHDOG,
        watchdogs.len(),
        cpu_freq_boost.len(),
        caches.len(),
        rngs.len(),
        bank_switch.len(),
        memory_layouts.len(),
        suffixes.len(),
        with_ota,
        with_config,
        config_keeps_ota,
        with_external,
    );

    let stamp = format_source_stamp(&declared_embassy_version(&repo_root)?, &probe_rs_version()?);
    let mut files = vec![
        ("chips.rhai".to_owned(), format_chip_list(&suffixes, &stamp)),
        (
            "packages.rhai".to_owned(),
            format_package_choices(&package_choices),
        ),
        ("bank_mode.rhai".to_owned(), format_bank_modes(&bank_modes)),
        ("watchdog.rhai".to_owned(), format_watchdogs(&watchdogs)),
        (
            "cpu_freq_boost.rhai".to_owned(),
            format_cpu_freq_boost(&cpu_freq_boost),
        ),
        (
            "bank_switch.rhai".to_owned(),
            format_bank_switch(&bank_switch),
        ),
        ("interfaces.rhai".to_owned(), format_interfaces(&interfaces)),
        ("instances.rhai".to_owned(), format_instances(&instances)),
        ("rng.rhai".to_owned(), format_rngs(&rngs)),
        ("cache.rhai".to_owned(), format_caches(&caches)),
    ];
    files.extend(
        format_memory_layouts(&memory_layouts)
            .into_iter()
            .map(|(family, body)| (format!("layout/{family}.rhai"), body)),
    );
    write_data_dir(&repo_root.join(DATA_DIR), &files)?;
    println!("{DATA_DIR}/ обновлён.");
    Ok(())
}

fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir
}

/// Временная копия всего репозитория (без `target/`/`.git`/самого
/// `chip-data-gen/`) с замененным `{{chip_feature}}` в `crates-cross/Cargo.toml` —
/// `cargo metadata` не может разрешить зависимости, пока там стоит буквальный
/// нерезолвящийся плейсхолдер. Копируем репозиторий целиком, а не только
/// `crates-cross/`: у `crates-cross` путь на `domain` относительный (`../crates-host/domain`),
/// поэтому структура каталогов вокруг `crates-cross/Cargo.toml` должна совпадать с
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

        let manifest_path = dir.join("crates-cross/Cargo.toml");
        let manifest = fs::read_to_string(&manifest_path)?;
        let patched = manifest.replace("{{chip_feature}}", PLACEHOLDER_CHIP_FEATURE);
        if patched == manifest {
            bail!(
                "{{{{chip_feature}}}} не найден в {} — плейсхолдер переименован в cargo-generate.toml/crates-cross/Cargo.toml?",
                manifest_path.display()
            );
        }
        fs::write(&manifest_path, patched)?;
        strip_liquid_from_manifests(&dir)?;

        Ok(Self { dir })
    }

    fn cross_manifest_path(&self) -> PathBuf {
        self.dir.join("crates-cross/Cargo.toml")
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
/// а те содержат условные куски (`"dual-core"` в `crates-cross/bsp`/`crates-cross/boot`,
/// `"boot"` среди `members` и фича банка в `crates-cross/Cargo.toml`) — с ними это
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

/// Формы с управлением пробелами (`{%-`, `-%}`) приводятся к обычным до
/// разбора. Без этого условие в `crates-cross/bsp/Cargo.toml`, записанное как
/// `{%- if ota == "true" %}`, не находилось вовсе — и `cargo metadata` падал
/// на «invalid key-value pair, expected key», то есть генератор данных
/// переставал работать целиком. Ловится это только запуском самого
/// генератора (он нужен редко — при обновлении embassy-stm32 или probe-rs),
/// поэтому здесь ещё и тесты на все формы.
///
/// Вложенность понимается: `{% endif %}` ищется парный, а не ближайший (см.
/// [`matching_endif`]). Раньше здесь стояло «вложенных `{% if %}` в шаблоне
/// нет и не должно появиться» со ссылкой на тест, которого в репозитории не
/// было, — и вложенность приехала незамеченной ровно туда, куда её
/// естественно писать: условная фича внутри `features = [...]` у
/// `embassy-boot`. Стриппер тогда резал от внешнего `{% if %}` до внутреннего
/// `{% endif %}`, оставлял лишний закрывающий тег, и генератор умирал на
/// `cargo metadata`.
fn strip_liquid(text: &str) -> String {
    let text = &text.replace("{%-", "{%").replace("-%}", "%}");
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(start) = rest.find(LIQUID_IF) {
        let after = &rest[start..];
        let Some(end) = matching_endif(after) else {
            // Незакрытый тег — оставляем остаток как есть, пусть падает
            // `cargo metadata` с понятной ошибкой, а не мы с невнятной.
            //
            // Текст до тега дописывается ПОСЛЕ этой проверки, а не до неё:
            // иначе на выходе он оказывался дважды — один раз здесь, второй
            // вместе с необрезанным `rest` в конце функции. То есть «оставим
            // как есть» на деле портило файл сильнее, чем сам незакрытый тег.
            break;
        };
        out.push_str(&rest[..start]);
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// Начало условного тега. Не `{% if %}` целиком: между `if` и `%}` стоит само
/// условие.
const LIQUID_IF: &str = "{% if";
const LIQUID_ENDIF: &str = "{% endif %}";

/// Смещение сразу за `{% endif %}`, парным к `{% if %}` в начале `text`.
///
/// `None` — тег не закрыт; тогда [`strip_liquid`] оставляет остаток как есть.
///
/// # Panics
///
/// Не паникует, но ждёт, что `text` начинается с [`LIQUID_IF`]: с этого и
/// начинается счёт глубины. Единственный вызывающий это обеспечивает.
fn matching_endif(text: &str) -> Option<usize> {
    debug_assert!(
        text.starts_with(LIQUID_IF),
        "не с открывающего тега: {text}"
    );

    let mut depth = 1usize;
    let mut at = LIQUID_IF.len();
    while depth > 0 {
        let endif = at + text[at..].find(LIQUID_ENDIF)?;
        let nested = text[at..].find(LIQUID_IF).map(|offset| at + offset);
        if let Some(nested) = nested
            && nested < endif
        {
            depth += 1;
            at = nested + LIQUID_IF.len();
        } else {
            depth -= 1;
            at = endif + LIQUID_ENDIF.len();
        }
    }
    Some(at)
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

/// Результат разбора `cargo metadata` для пропатченного `crates-cross/Cargo.toml`:
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
        .context("не удалось запустить `cargo metadata` для crates-cross/Cargo.toml")?;
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
        .context("embassy-stm32 не найден среди зависимостей crates-cross/Cargo.toml")?;
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
        .context("stm32-metapac не найден среди зависимостей crates-cross/Cargo.toml")?;
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
    flash: Option<FlashSettings>,
}

/// Та часть `FlashSettings` из `stm32-metapac`, которая нужна шаблону.
///
/// Именованные поля, а не кортеж: третье значение (`erase_value`) не читается
/// по позиции никак, а два первых уже путались местами при чтении кода.
#[derive(Clone, Copy)]
struct FlashSettings {
    erase_size: u64,
    write_size: u64,
    /// Чем читается СТЁРТАЯ страница: `0xFF` у подавляющего большинства
    /// семейств, `0x00` у L0/L1 и STM32WB10CC/WB15CC.
    ///
    /// От этого зависит фича `flash-erase-zero` у `embassy-boot` (без неё
    /// обмен разделов читает признак «стёрто» наоборот) и то, работает ли
    /// раздел настроек вообще (`sequential-storage` ищет заголовок записи как
    /// «все байты `0xFF`»). Обе поломки молчаливые, поэтому значение берётся
    /// из метаданных, а не выводится по имени чипа.
    erase_value: u8,
}

/// Виды регионов в метаданных `stm32-data` — ровно те, что есть в
/// `MemoryRegionKind`.
///
/// Без варианта «прочее» намеренно: раньше неузнанное имя становилось `Other`,
/// а `compute_memory_layout` отбирает flash-регионы по `kind == Flash` — то
/// после переименования `MemoryRegionKind::Flash` в `stm32-data` раскладка
/// памяти перестала бы считаться для ВСЕХ чипов, и `cargo generate` не упал бы:
/// единственным следом осталась бы цифра в сводке, на которую никто не смотрит.
#[derive(PartialEq, Eq, Debug)]
enum RegionKind {
    Flash,
    Ram,
    /// L0/L1 — блок EEPROM, читается напрямую, пишется через контроллер flash.
    Eeprom,
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

/// Как у этого чипа зовётся блок независимого сторожевого таймера:
/// `IWDG`, `IWDG1` или `IWDG2`.
///
/// Нужно потому, что `bsp` объявляет пару псевдонимов
/// `wdg::BoardWatchdog`/`UnarmedBoardWatchdog` над
/// `Iwdg<peripherals::IWDG…, HW_TIMEOUT_US, Armed|Disarmed>` и строит из блока
/// поле `Board`, а имя типа в `embassy-stm32` — то самое,
/// что стоит в метаданных: на большинстве STM32 `IWDG`, на H7 `IWDG1`, на
/// двухъядерных H7 ещё и `IWDG2`. Угадывать его по имени чипа нельзя — ровно
/// на такой эвристике уже ошибся признак `erase_zero` (см. docs/flash.md), а
/// метаданные знают ответ точно.
///
/// Читается не из `<chip>/metadata.rs`: там лежат только имя, семейство и
/// карта памяти, а перечень блоков — в общем файле (см. [`shared_metadata`]).
///
/// Когда блоков два (двухъядерные H7: `IWDG1` у ядра CM7, `IWDG2` у CM4),
/// метаданные обоих ядер перечисляют оба — различить их там нечем. Выбор
/// делается по суффиксу ядра, который каскад УЖЕ выбрал (`-cm4`), а не по
/// имени чипа: это не догадка о железе, а перенос уже сделанного выбора
/// туда, где метаданные молчат.
fn chip_watchdog(
    suffix: &str,
    shared: &SharedMetadata,
    shared_path: &Path,
) -> anyhow::Result<String> {
    let names = &shared.watchdogs;
    match names.len() {
        0 => bail!("{}: не найдено ни одного блока IWDG", shared_path.display()),
        1 => Ok(names[0].clone()),
        _ if suffix.ends_with("-cm4") => Ok(names[names.len() - 1].clone()),
        _ => Ok(names[0].clone()),
    }
}

/// Что генератор берёт из общего файла метаданных — того, который
/// `<chip>/metadata.rs` включает первой строкой
/// (`include!("../metadata_0180.rs")`): перечень блоков живёт там, а не в
/// файле чипа.
struct SharedMetadata {
    /// Имена блоков `IWDG*` — см. [`chip_watchdog`].
    watchdogs: Vec<String>,
    /// Блок `PWR` версии `h7rm0468` (STM32H723/725/730/733/735): только у этой
    /// линии потолок ядра на VOS0 зависит от option byte `CPU_FREQ_BOOST` —
    /// 520 МГц без него, 550 МГц с ним. По той же версии `embassy-stm32`
    /// выбирает предел частоты (`cfg(pwr_h7rm0468)` в `rcc/h.rs`), так что
    /// признак и HAL расходиться не могут.
    cpu_freq_boost: bool,
    /// Версия регистров блока `FLASH` (`g4c3`, `f4`, `l5`...).
    flash_version: Option<String>,
    /// Интерфейсы связи, выведенные в корпусе, — см. [`parse_interfaces`].
    interfaces: Vec<&'static str>,
    /// Имена блоков, которыми `resources.rs` называет каждый интерфейс, — см.
    /// [`parse_instances`].
    instances: BTreeMap<&'static str, Vec<String>>,
    /// Прерывание блока `RNG` — см. [`parse_rng_interrupt`].
    rng_interrupt: Option<String>,
    /// Есть блок `CRS` — по нему `embassy-stm32` включает HSI48 (см. [`rng_clock`]).
    has_crs: bool,
    /// Есть блок `ICACHE` (L5, U3/U5, H5, WBA, H7R/S) — кэш инструкций вне ядра,
    /// включается битом `ICACHE_CR.EN`.
    has_icache: bool,
}

/// Путь к общему файлу метаданных чипа и разобранные из него факты. Общий файл
/// делят десятки чипов, поэтому разбор кешируется в `cache` по пути.
fn shared_metadata<'c>(
    chips_dir: &Path,
    suffix: &str,
    cache: &'c mut BTreeMap<PathBuf, SharedMetadata>,
) -> anyhow::Result<(PathBuf, &'c SharedMetadata)> {
    let path = chips_dir.join(format!("stm32{suffix}")).join("metadata.rs");
    let text = fs::read_to_string(&path)
        .with_context(|| format!("не удалось прочитать {}", path.display()))?;
    let included = parse_included_metadata(&text)
        .with_context(|| format!("в {} нет строки include!", path.display()))?;
    let shared_path = path
        .parent()
        .expect("у пути к metadata.rs всегда есть каталог чипа")
        .join(included);

    if !cache.contains_key(&shared_path) {
        let shared_text = fs::read_to_string(&shared_path)
            .with_context(|| format!("не удалось прочитать {}", shared_path.display()))?;
        cache.insert(
            shared_path.clone(),
            SharedMetadata {
                watchdogs: parse_watchdog_names(&shared_text),
                cpu_freq_boost: has_pwr_rm0468(&shared_text),
                flash_version: registers_version(&shared_text, "flash"),
                interfaces: parse_interfaces(&shared_text),
                instances: parse_instances(&shared_text),
                rng_interrupt: parse_rng_interrupt(&shared_text),
                has_crs: shared_text.contains("kind: \"crs\""),
                has_icache: shared_text.contains("kind: \"icache\""),
            },
        );
    }
    let facts = &cache[&shared_path];
    Ok((shared_path, facts))
}

/// Кэши чипа, которые включает `bsp::cache` (кроме кэшей ядра Cortex-M7 — их
/// шаблон выбирает по `cpu`): `icen`/`dcen` — кэши инструкций и данных
/// акселератора флеша (биты `FLASH_ACR`), `icache` — блок `ICACHE`.
///
/// Биты `FLASH_ACR` берутся из файла регистров версии блока `FLASH` в
/// `stm32-metapac`, а не из таблицы семейств: метаданные чипа перечисляют блоки
/// и их версии, поля регистров лежат только там. Кэш данных `DCACHE` (U5, H5)
/// не включается: он стоит перед внешней памятью (`OCTOSPI`/`FMC`), а её у
/// шаблона нет.
fn chip_caches(chips_dir: &Path, shared: &SharedMetadata) -> anyhow::Result<Vec<&'static str>> {
    let mut found = Vec::new();
    if let Some(version) = &shared.flash_version {
        let path = chips_dir
            .parent()
            .context("у каталога chips нет родителя")?
            .join("peripherals")
            .join(format!("flash_{version}.rs"));
        let registers = fs::read_to_string(&path)
            .with_context(|| format!("не удалось прочитать {}", path.display()))?;
        for (token, setter) in [("icen", "pub fn set_icen("), ("dcen", "pub fn set_dcen(")] {
            if registers.contains(setter) {
                found.push(token);
            }
        }
    }
    if shared.has_icache {
        found.push("icache");
    }
    Ok(found)
}

/// Версия регистров блока вида `kind` — строка за `kind: "<kind>",` и
/// `version: "`.
fn registers_version(text: &str, kind: &str) -> Option<String> {
    let rest = text.split_once(&format!("kind: \"{kind}\","))?.1;
    let rest = rest.trim_start().strip_prefix("version: \"")?;
    rest.split_once('"').map(|(version, _)| version.to_string())
}

/// Есть ли в метаданных блок `PWR` с регистрами версии `h7rm0468`.
///
/// Матч по паре строк `kind: "pwr"` / `version: "h7rm0468"` подряд: версия
/// `h7rm0468` бывает и у других блоков (`rcc`, `syscfg` той же линии), а
/// предел частоты держит именно `pwr`.
fn has_pwr_rm0468(text: &str) -> bool {
    text.split("kind: \"pwr\",")
        .skip(1)
        .any(|rest| rest.trim_start().starts_with("version: \"h7rm0468\""))
}

/// Интерфейсы связи чипа — по блокам, у которых в корпусе есть выводы, в
/// порядке [`INTERFACE_TAGS`] и без повторов.
///
/// Блок без выводов (`pins: &[]`) в метаданных бывает: кристалл тот же, а в
/// мелком корпусе линии не выведены, — и такой интерфейс чипу недоступен.
/// Метка — то, что различает код в `bsp`: у FDCAN и bxCAN разные драйверы
/// `embassy-stm32`, у USB OTG и USB device — разные конструкторы.
fn parse_interfaces(text: &str) -> Vec<&'static str> {
    let mut found: Vec<&'static str> = Vec::new();
    for block in text.split("Peripheral {").skip(1) {
        if let Some(tag) = interface_block(block).map(|(tag, _)| tag)
            && !found.contains(&tag)
        {
            found.push(tag);
        }
    }
    found.sort_by_key(|tag| INTERFACE_TAGS.iter().position(|t| t == tag));
    found
}

/// Метка интерфейса и имя блока, если блок — интерфейс связи с выводами в
/// корпусе: у `usart`/`fdcan`/`bxcan` выведены `TX` и `RX`, у `usb`/`otg` —
/// `DP` и `DM`, у `eth` — хоть один вывод. Общая для [`parse_interfaces`] и [`parse_instances`]: метка и имя
/// не должны расходиться.
fn interface_block(block: &str) -> Option<(&'static str, &str)> {
    let kind = field(block, "kind").map(|v| v.trim_matches('"'));
    let version = field(block, "version").map(|v| v.trim_matches('"'));
    let tag = match (kind, version) {
        (Some("usart"), _) => "usart",
        (Some("can"), Some("bxcan")) => "bxcan",
        (Some("can"), Some(v)) if v.starts_with("fdcan") => "fdcan",
        (Some("eth"), _) => "eth",
        (Some("usb"), _) => "usb",
        (Some("otg"), _) => "otg",
        _ => return None,
    };
    let pins = block.split_once("pins: &[").map_or("", |(_, rest)| {
        rest.split_once(']').map_or(rest, |(p, _)| p)
    });
    let has = |signal: &str| pins.contains(&format!("signal: \"{signal}\""));
    let wired = match tag {
        "usart" | "fdcan" | "bxcan" => has("TX") && has("RX"),
        "usb" | "otg" => has("DP") && has("DM"),
        _ => !pins.trim().is_empty(),
    };
    let name = field(block, "name").map(|v| v.trim_matches('"'))?;
    wired.then_some((tag, name))
}

/// Какие блоки чипа называет `resources.rs` для каждой метки интерфейса.
///
/// Метка говорит, что на чипе есть блок такого рода, но не как он зовётся:
/// у STM32F030F4 нет `USART2` (только `USART1`), а у STM32H723 нет
/// `USB_OTG_FS` (только `USB_OTG_HS`). Имя нужно проекту, чтобы положить блок
/// в группу `assign_resources!`, — выбор по соглашению не собрался бы у
/// десятков чипов. Берутся все блоки с выводами нужных сигналов в корпусе
/// (как в [`parse_interfaces`]), в порядке выбора: предпочтительный
/// (`USART*` раньше `UART*`/`LPUART*`, `USB_OTG_FS` раньше `USB_OTG_HS`,
/// `CAN1` раньше прочих), а дальше — с меньшим номером. Первое имя — тот
/// блок, который проект получает по умолчанию.
fn parse_instances(text: &str) -> BTreeMap<&'static str, Vec<String>> {
    let mut ranked: BTreeMap<&'static str, Vec<(u8, usize, String)>> = BTreeMap::new();
    for block in text.split("Peripheral {").skip(1) {
        let Some((tag, name)) = interface_block(block) else {
            continue;
        };
        let preferred = match tag {
            "usart" => name.starts_with("USART"),
            "otg" => name == "USB_OTG_FS",
            "bxcan" => name == "CAN1" || name == "CAN",
            _ => true,
        };
        ranked
            .entry(tag)
            .or_default()
            .push((u8::from(!preferred), name.len(), name.to_owned()));
    }
    ranked
        .into_iter()
        .map(|(tag, mut blocks)| {
            blocks.sort();
            (tag, blocks.into_iter().map(|(_, _, name)| name).collect())
        })
        .collect()
}

/// Прерывание блока `RNG`: у разных линий оно своё и бывает общим с
/// соседями (`RNG`, `HASH_RNG`, `AES_RNG`, `RNG_LPUART1`, `RNG_CRYP`...), а
/// `bind_interrupts!` в `bsp` называет его по имени. `None` — блока нет или
/// у него нет прерывания (STM32WB0).
fn parse_rng_interrupt(text: &str) -> Option<String> {
    text.split("Peripheral {").skip(1).find_map(|block| {
        if field(block, "kind")?.trim_matches('"') != "rng" {
            return None;
        }
        let rest = block.split_once("interrupt: \"")?.1;
        rest.split_once('"')
            .map(|(interrupt, _)| interrupt.to_owned())
    })
}

/// Что добавить к `embassy_stm32::Config::default()`, чтобы у блока `RNG`
/// был клок: способ (`bsp::board` разворачивает его в строку конфига) и
/// вариант мультиплексора, если способ — мультиплексор.
type RngClock = (&'static str, &'static str);

/// Клок `RNG` по линии чипа (RM каждой линии, раздел RCC).
///
/// В метаданных этого нет: у половины линий источник RNG там записан шиной
/// (`HCLK2`), хотя на деле блок тактируется от 48 МГц, а мультиплексоры
/// `embassy-stm32` по умолчанию пишет нулями, и ноль у части линий — «без
/// клока» (`RNGSEL` у G0) или выключенный PLL (`RNGSEL` у WL). Без клока
/// `Rng::new` у линий с мультиплексором паникует на старте, а у остальных
/// (F2/F4/F7, L0, L4, U0) виснет: внутри он ждёт первое слово. Поэтому
/// линия, источник которой не проверен по RM, не получает RNG вовсе (`None`)
/// — у неё затравка остаётся из заводского номера: F410 (свой RCC), H7R/H7S,
/// STM32WB0, U031 (`CLK48SEL` нет в метаданных). Источник HSI48 к тому же
/// требует CRS — это проверяет вызывающий.
fn rng_clock(suffix: &str) -> Option<RngClock> {
    let line = |prefixes: &[&str]| prefixes.iter().any(|p| suffix.starts_with(p));
    if line(&["f410", "h7r", "h7s", "wb0", "u031"]) {
        None
    } else if line(&["f2", "f4", "f7"]) {
        // PLL48CK — выход Q основного PLL; по умолчанию PLL выключен.
        Some(("pll48", ""))
    } else if line(&["g0"]) {
        // `RNGSEL` = 0 — клока нет.
        Some(("rngsel", "SYS"))
    } else if line(&["wba"]) {
        // `RNGSEL` = 0 — LSE, по умолчанию выключенный.
        Some(("rngsel", "HSI"))
    } else if line(&["wl"]) {
        // `RNGSEL` = 0 — PLLQ, а PLL выключен; MSI включён по умолчанию.
        Some(("rngsel", "MSI"))
    } else if line(&["l0", "u0"]) {
        // `CLK48SEL` = 0 — PLL (L0) или клока нет (U0); HSI48 включён.
        Some(("clk48sel", "HSI48"))
    } else if line(&["l471", "l475", "l476", "l486"]) {
        // HSI48 у этих L4 нет, и `CLK48SEL` = 0 у них — «без клока».
        Some(("clk48sel", "MSI"))
    } else if line(&["g4", "h5", "h7", "l4", "l5", "u5", "wb"]) {
        // Ноль мультиплексора — HSI48, а его `embassy-stm32` включает сам.
        Some(("default", ""))
    } else {
        None
    }
}

/// Метки интерфейсов в таблице `INTERFACES` — в этом порядке.
const INTERFACE_TAGS: [&str; 6] = ["usart", "fdcan", "bxcan", "eth", "usb", "otg"];

/// Путь из `include!("../metadata_0180.rs")` — первой строки любого
/// `<chip>/metadata.rs`.
fn parse_included_metadata(text: &str) -> Option<&str> {
    let rest = text.split_once("include!(\"")?.1;
    rest.split_once('"').map(|(path, _)| path)
}

/// Имена блоков `IWDG*` в порядке появления, без повторов.
///
/// Матч намеренно грубый — по литералу `name: "IWDG`, без разбора того,
/// перечень это блоков или прерываний: у прерывания то же имя, что у блока,
/// а расхождения между этими двумя списками не бывает (проверено по всем 891
/// наборам метаданных: набор `{IWDG}`, `{IWDG1}` или `{IWDG1, IWDG2}`, и ни
/// одного, где `IWDG` соседствовал бы с `IWDG1`).
fn parse_watchdog_names(text: &str) -> Vec<String> {
    let mut names: Vec<String> = Vec::new();
    for chunk in text.split("name: \"IWDG").skip(1) {
        let Some((tail, _)) = chunk.split_once('"') else {
            continue;
        };
        let name = format!("IWDG{tail}");
        if !names.contains(&name) {
            names.push(name);
        }
    }
    names
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
            let kind = region_kind(block)?;
            let address_str = field(block, "address").context("MemoryRegion без address")?;
            let address = u64::from_str_radix(address_str.trim_start_matches("0x"), 16)
                .with_context(|| format!("не число: address = {address_str}"))?;
            let size: u64 = field(block, "size")
                .context("MemoryRegion без size")?
                .parse()
                .context("не число: size")?;
            let flash = if block.contains("settings: None") {
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
                let erase_value: u8 = field(block, "erase_value")
                    .context("FlashSettings без erase_value")?
                    .parse()
                    .context("не число: erase_value")?;
                Some(FlashSettings {
                    erase_size,
                    write_size,
                    erase_value,
                })
            };
            Ok(RawRegion {
                name,
                kind,
                address,
                size,
                flash,
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
/// Из двух режимов выбирается одобанковый: флеш в нём непрерывен, и
/// `ACTIVE`/`DFU` делят его пополам — прошивка выходит максимально возможного
/// размера. У двухбанкового чипа с разрывом между банками (G47x с 128/256 KiB)
/// приложению достался бы только банк 1, а у стыкующихся банков страница
/// вдвое мельче, но раскладка та же. С завода одобанковый режим стоит у
/// F42x/F43x/F46x/F47x с 1 MiB и F76x/F77x; у G4 cat3, L4+, L5 и G0B/G0C — нет,
/// и прошивка переключает его сама при первом старте (docs/flash.md):
/// одобанковая раскладка на чипе в двухбанковом режиме не работает вовсе —
/// `embassy-stm32` на G0/G4 паникует в `Flash::new`, а на L4+ молча стирает
/// страницы не того размера.
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

/// Вид региона по полю `kind` — строго, без «прочее».
///
/// Сопоставление точное (после префикса `MemoryRegionKind::`): оно не
/// переживает переименование варианта: `MemoryRegionKind::FlashBank1` дал бы «прочее», а
/// [`compute_memory_layout`] отбирает flash-регионы по `kind == Flash` — и
/// раскладка памяти молча перестала бы считаться у всех чипов разом. Именно
/// поэтому неизвестное имя здесь ошибка, а не значение по умолчанию.
fn region_kind(block: &str) -> anyhow::Result<RegionKind> {
    let raw = field(block, "kind").context("MemoryRegion без kind")?;
    // Имя печатается как `MemoryRegionKind::Flash`; префикс типа отбрасываем,
    // остальное сверяем ТОЧНО. Суффиксное сопоставление (`ends_with("Ram")`)
    // приняло бы за RAM любой будущий `BackupRam`/`CcmRam` — и flash/RAM-цепочки
    // молча получили бы лишний регион.
    let name = raw.strip_prefix("MemoryRegionKind::").unwrap_or(raw);
    Ok(match name {
        "Flash" => RegionKind::Flash,
        "Ram" => RegionKind::Ram,
        "Eeprom" => RegionKind::Eeprom,
        other => bail!(
            "неизвестный вид региона `{other}`: в stm32-data их ровно три — Flash, Ram, \
             Eeprom. Если вариант переименовали, починить надо здесь и в [`RegionKind`], а не \
             оставлять чипы без посчитанного memory.x"
        ),
    })
}

/// Значение поля `{key}: ...` до ближайшей запятой/переноса строки внутри
/// одного `block` (текст между `MemoryRegion {` и следующим таким же
/// заголовком, см. `parse_chip_memory`) — этого достаточно, все нужные поля
/// здесь плоские (`name: "..."`, `address: 0x...`, `size: ...`), кроме
/// вложенного `FlashSettings { erase_size: ..., write_size: ... }`, у
/// которого поля тоже плоские и не пересекаются по имени с полями снаружи.
///
/// **Совпадение ищется по границе поля, а не по подстроке.** Иначе `size: `
/// матчит `erase_size: `, и всё работает только потому, что `stm32-metapac`
/// печатает поля в порядке `name, kind, address, size, settings`: переезд
/// `settings` выше `size` вернул бы `2048` вместо размера региона, и
/// `flash_total` оказался бы неверным у сотен чипов — полностью молча, потому
/// что `parse` такой результат принимает.
fn field<'a>(block: &'a str, key: &str) -> Option<&'a str> {
    let pat = format!("{key}: ");
    let mut from = 0;
    while let Some(found) = block[from..].find(&pat) {
        let start = from + found;
        // Перед полем — либо начало строки, либо `{`/`,` c пробелами: все
        // варианты, где `key` начинает поле. `_` и буква означают, что это
        // суффикс чужого поля (`erase_size` для `size`), и такой матч пропускаем.
        let starts_field = block[..start]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric() && c != '_');
        if starts_field {
            let rest = &block[start + pat.len()..];
            // Закрывающие скобки — такая же граница значения, как запятая и
            // перевод строки. В самих значениях (числа, адреса,
            // `MemoryRegionKind::Flash`, имена в кавычках) их не бывает, а вот
            // последнее поле блока запятой не заканчивается: `erase_value: 255
            // })` без них возвращалось целиком, вместе с хвостом, и `parse`
            // падал на «invalid digit». В файлах `stm32-metapac` поля
            // печатаются по одному на строку, поэтому наружу это не вылезало —
            // поймал тест на сжатой в одну строку записи.
            let end = rest.find([',', '\n', '}', ')'])?;
            return Some(rest[..end].trim());
        }
        from = start + pat.len();
    }
    None
}

/// Партиции OTA-схемы (`crates-cross/boot`). Есть только у чипов, где схема
/// физически помещается — см. `compute_ota_partitions`.
#[derive(Debug)]
struct OtaPartitions {
    /// `FLASH` в `crates-cross/boot/memory.x`: место под сам бинарник bootloader'а,
    /// от базы flash до `BOOTLOADER_STATE`. Отдельный регион (а не весь чип)
    /// нужен, чтобы переполнение ловил линкер, а не молчаливое наложение на
    /// `ACTIVE`.
    bootloader_length: u64,
    bootloader_state_origin: u64,
    bootloader_state_length: u64,
    /// Сектор, в котором лежит `BOOTLOADER_STATE`, — страница, которую ему
    /// объявляет `adapters::flash::ErasePage` (см. `compute_ota_partitions`, пятая
    /// проверка). Меньше `PAGE_SIZE` на чипах с неравными секторами.
    bootloader_state_erase_size: u64,
    /// Страница обмена embassy-boot (`PAGE_SIZE`): `ACTIVE`/`DFU` кратны ей, а
    /// `DFU` длиннее `ACTIVE` ровно на неё. Больше физической страницы чипа
    /// (`MemoryLayout::page_size`), когда прошивка оборачивает `ACTIVE`/`DFU` в
    /// `adapters::flash::ErasePage<_, SWAP_PAGE_SIZE>` — см. [`compute_ota_partitions`].
    swap_page_size: u64,
    active_origin: u64,
    active_length: u64,
    dfu_origin: u64,
    dfu_length: u64,
    /// Раздел настроек, если схема считалась вместе с ним (набор `config_`):
    /// в мелких секторах перед `ACTIVE` или в хвосте flash — что оставляет
    /// `ACTIVE` больше (см. `compute_config_partition`). `None` — без настроек.
    settings: Option<SettingsPartition>,
}

/// Где лежит раздел настроек и какая у него страница (`adapters::flash::ErasePage`
/// в `bsp::settings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SettingsPartition {
    origin: u64,
    length: u64,
    erase_size: u64,
}

/// Готовые для `memory.x` адреса/размеры одного чипа. Всё в байтах —
/// `format_memory_layouts` переводит в hex/`K`-нотацию для Rhai.
struct MemoryLayout {
    flash_origin: u64,
    flash_length: u64,
    ram_origin: u64,
    ram_length: u64,
    /// Хвост RAM под дамп `panic-persist` — единственный резерв в RAM. Всегда
    /// есть, если есть раскладка: без символов `_panic_dump_*` release-образ
    /// `app` не слинкуется (см. PANIC_MIN).
    panic: (u64, u64),
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
    /// Физическая страница чипа: максимальный сектор стирания по цепочке
    /// flash. Страница обмена `embassy-boot` (`PAGE_SIZE`) — она же, пока
    /// прошивка не расширила её (`OtaPartitions::swap_page_size`, плейсхолдер
    /// `{{page_size}}`): по ней bootloader переносит разделы, а host-target
    /// тест ищет, куда уехал прежний образ — `ACTIVE[i]` оказывается в
    /// `DFU[i + 1]`, то есть со сдвигом ровно на страницу обмена.
    page_size: u64,
    /// Стёртая страница читается нулями, а не `0xFF`.
    ///
    /// Берётся из `erase_value` в метаданных, а не выводится по имени чипа, и
    /// это не педантизм: раньше признак считался как «фича начинается на
    /// `l0`/`l1`», и под эвристику не попадали STM32WB10CC/WB15CC — у них
    /// `erase_value: 0` при совсем другом префиксе. Такому чипу доставался
    /// `embassy-boot` без фичи `flash-erase-zero` (обмен разделов читает
    /// признак «стёрто» наоборот) и раздел настроек, который не работает
    /// вовсе. Обе поломки молчаливые.
    erase_zero: bool,
    /// Начало второго банка, если между банками разрыв (H743 с 1 MiB и т.п.).
    /// Цельный `Flash` embassy туда не дотягивается — его `FLASH_SIZE` равен
    /// сумме банков, а смещение банка 2 больше неё, — и `DFU` берётся через
    /// регион банка (`Bank2Region`), со смещениями от этого адреса.
    dfu_bank_origin: Option<u64>,
    /// Non-cacheable память под буферы DMA — см. [`dma_nc_region`]; из блока-донора
    /// в `extra_regions` (или из `ram_length`) она уже вычтена.
    dma_nc: Option<DmaNc>,
    /// `SRAM4` целиком под буферы BDMA (`origin`, `length`) — см. [`bdma_nc_region`].
    bdma_nc: Option<(u64, u64)>,
    /// `Ok` — OTA-схема помещается в flash; `Err` — не помещается, и текст
    /// объясняет почему (печатается при генерации и попадает комментарием в
    /// `memory.x`). Во втором случае проект собирается одним образом на весь
    /// flash, без `crates-cross/boot`.
    ota: Result<OtaPartitions, String>,
    /// Раздел под настройки, если он на этом чипе вообще возможен. `None` —
    /// две страницы стирания не остаются даже без всего остального.
    config: Option<ConfigPartition>,
    /// OTA с `DFU` и `STATE` на внешней флеши: внутри только bootloader и
    /// `ACTIVE`. `Err` — даже так не помещается; текст объясняет почему.
    external: Result<ExternalOta, String>,
    /// То же, но настройки — во внутреннем флеше (`ExternalOta::settings`):
    /// когда проект выбрал их там, а не на внешней флеши.
    external_config: Result<ExternalOta, String>,
}

/// Раскладка того же чипа, но с отрезанным разделом под настройки.
///
/// Отдельным набором, а не поправкой к основному: OTA-схема считается ОТ
/// размера доступного flash (`compute_ota_partitions`), поэтому при
/// уменьшении flash на две страницы `ACTIVE`/`DFU` съезжают целиком. Считать
/// это в rhai-хуке значило бы повторить там все четыре проверки
/// `assert_partitions` из `embassy-boot`.
struct ConfigPartition {
    /// Раздел в хвосте flash — так он лежит в проекте без OTA: таблица векторов
    /// приложения стоит в секторе 0, и раздел посреди `FLASH` разорвал бы его.
    tail: SettingsPartition,
    /// Что остаётся приложению, когда OTA не используется: весь flash минус
    /// раздел настроек.
    flash_length: u64,
    /// OTA-схема вместе с разделом; где в ней сам раздел — `settings`.
    ota: Result<OtaPartitions, String>,
}

/// OTA с разделами на внешней флеши: во внутренней — bootloader и `ACTIVE`
/// (он и обязан быть внутри: из него исполняется код), на внешней — `STATE`,
/// `DFU` и, если нужен, раздел настроек.
///
/// Геометрию внешней микросхемы генератор не знает — её несёт драйвер,
/// который подставит проект. Поэтому всё внешнее выровнено с запасом
/// (`ext_align`), а сколько раскладка требует от драйвера, сказано явно
/// (`page_size`): его `ERASE_SIZE` обязан этот размер делить. Проверяет это
/// `const`-assert рядом с драйвером, а не генератор.
#[derive(Debug)]
struct ExternalOta {
    /// `FLASH` в `crates-cross/boot/memory.x`: от базы flash до `ACTIVE` или до
    /// раздела настроек, если тот лёг в зону bootloader'а.
    bootloader_length: u64,
    active_origin: u64,
    active_length: u64,
    /// `PAGE_SIZE` обмена в предположении, что стирание драйвера его делит:
    /// крупнейший сектор чипа, но не меньше `EXT_ERASE_MIN`.
    page_size: u64,
    /// Шаг, по которому выровнены смещения и размеры на внешней флеши.
    ext_align: u64,
    /// Смещения — от начала внешней флеши, а не адреса.
    state_offset: u64,
    state_length: u64,
    dfu_offset: u64,
    dfu_length: u64,
    /// Раздел настроек на внешней флеши — сразу за `DFU`; две страницы по
    /// `ext_align`. Используется, только если настройки выбраны.
    config_offset: u64,
    config_length: u64,
    /// Раздел настроек во ВНУТРЕННЕМ флеше — только у набора `ext_cfg_`.
    settings: Option<SettingsPartition>,
}

/// Самое мелкое стирание, которое раскладка ждёт от внешней флеши: сектор
/// 4 KiB есть у последовательной NOR всех крупных производителей (W25Q, MX25,
/// GD25, IS25, AT25). `ACTIVE` кратен ему: обмен идёт страницами
/// `max(ACTIVE::ERASE_SIZE, DFU::ERASE_SIZE)`, и драйвер с более крупным
/// стиранием потребовал бы `ACTIVE` кратного уже его странице.
const EXT_ERASE_MIN: u64 = 4 * 1024;
/// Шаг разделов на внешней флеши: блок 64 KiB — самое крупное стирание,
/// которое бывает у той же NOR. Внешняя память дешёвая, и запас здесь ничего
/// не стоит, а разделы остаются годными для драйвера с любым стиранием до
/// блока.
const EXT_ALIGN: u64 = 64 * 1024;
/// Самое широкое слово записи, на которое считается журнал `STATE`: то же,
/// что `adapters::ota::MAX_WRITE_SIZE`, — буфер шире адаптер всё равно не
/// держит.
const EXT_WRITE_MAX: u64 = 32;

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
/// ещё на страницу. 26 KiB: подписанный release-`boot` весит ≈23 KiB (подпись
/// обязательна при OTA), остальное — запас на рост.
const BOOTLOADER_MIN: u64 = 26 * 1024;
/// На сколько страниц резерв может вырасти СВЕРХ исходного (`BOOTLOADER_TARGET`,
/// округлённого вверх до страницы). Именно сверх, а не всего: у чипа с мелкими
/// секторами исходный резерв сам по себе занимает десятки страниц (32 KiB при
/// секторе 256 байт у L0/L1 — 128 штук), и абсолютный предел отрезал бы от OTA
/// всю мелкосекторную половину линейки. У крупных страниц предел — эти 16
/// страниц, у мелких его поднимает [`MAX_EXTRA_RESERVED_BYTES`].
const MAX_EXTRA_RESERVED_PAGES: u64 = 16;
/// Рост резерва не меньше этого объёма независимо от размера страницы: 16
/// страниц по 128 байт — всего 2 KiB, а `STATE` на такой странице может занять
/// и больше.
const MAX_EXTRA_RESERVED_BYTES: u64 = 8 * 1024;
/// Бюджет `BOOTLOADER_STATE` под журнал обмена: пока он не больше, страницу
/// обмена не расширяют. Журнал — `(2 + 4*N) * WRITE_SIZE` байт при N страницах
/// обмена в `ACTIVE` (`assert_partitions`), то есть растёт с flash и убывает с
/// размером страницы: L0 (страница 128 байт, ~640 страниц) без расширения
/// отдавал под `STATE` 10 KiB из 32 KiB, и подписанный bootloader не влезал.
const STATE_BUDGET: u64 = 2 * 1024;
/// Самая крупная страница обмена, до которой её расширяют ради `STATE_BUDGET`.
/// Больше — крупнее разменная страница `DFU` и грубее потери на хвосте flash.
const SWAP_PAGE_MAX: u64 = 8 * 1024;
/// От конца RAM отрезается регион `PANIC` под дамп `panic-persist` —
/// паникёр release-профиля пишет туда по голым адресам
/// `_panic_dump_start.._panic_dump_end`, ничего не зная о секциях (секция у
/// верхней границы RAM сломала бы `flip-link`, см. комментарий в
/// `chip-select.rhai`).
///
/// Размер — доля RAM, а не константа: у чипа с 2 KiB (STM32L011 и родня)
/// фиксированные полкилобайта были бы четвертью всей памяти. Регион нужен
/// каждому чипу: без символов дампа `panic-persist` не линкуется, а он стоит у
/// `app` и `boot` безусловно (в release).
const PANIC_FRACTION: u64 = 16;
/// Нижняя граница: восемь байт съедает заголовок дампа, дальше текст — на
/// «panicked at src/main.rs:42:5» хватает, длинное сообщение обрежется.
const PANIC_MIN: u64 = 128;
/// Больше полукилобайта отдавать незачем: сообщение всё равно обрезается по
/// размеру региона.
const PANIC_MAX: u64 = 512;

/// Размеры региона `DMA_NC` по убыванию. Регион MPU Armv7-M — степень двойки не
/// меньше 32 байт, адрес кратен размеру; берётся наибольший, при котором конец
/// блока-донора даёт такой адрес. 32 KiB хватает на очереди Ethernet и буферы
/// SDMMC, меньшие — запасной вариант для блоков с «кривым» концом.
const DMA_NC_SIZES: [u64; 4] = [32 * 1024, 16 * 1024, 8 * 1024, 4 * 1024];

/// Адрес `SRAM4` домена D3 у H7 — память, до которой дотягивается только BDMA.
const H7_SRAM4: u64 = 0x3800_0000;

/// Регион `DMA_NC`: non-cacheable память под буферы DMA, которую `bsp::dma_nc`
/// размечает в MPU. Без неё кэш данных ядра M7 DMA не видит.
#[derive(Debug, Clone, PartialEq, Eq)]
struct DmaNc {
    origin: u64,
    length: u64,
    /// Блок, от конца которого отрезан регион (`AXISRAM`, `SRAM1`); `None` —
    /// хвост основной цепочки RAM (F7: `DTCM` + `SRAM`).
    donor: Option<String>,
}

/// Ядро Cortex-M7 в этом шаблоне — семейства F7 и H7 (`family_table` в
/// `chip-select.rhai`), кроме ядра CM4 двухъядерных H7 (суффикс `-cm4`).
fn is_cortex_m7(chip: &str) -> bool {
    (chip.starts_with("f7") || chip.starts_with("h7")) && !chip.ends_with("-cm4")
}

/// Наибольший размер из [`DMA_NC_SIZES`], при котором конец блока `end` кратен
/// размеру, а сам блок длиной `block` не меньше четырёх таких.
fn dma_nc_size(end: u64, block: u64) -> Option<u64> {
    DMA_NC_SIZES
        .into_iter()
        .find(|&size| block >= 4 * size && end.is_multiple_of(size))
}

/// Где взять регион `DMA_NC` у чипа с кэшем данных ядра (M7).
///
/// По порядку: `AXISRAM` (H7), `SRAM1` в окне AXI-RAM `0x24000000` (H7R/S), хвост
/// основной цепочки RAM (F7: `DTCM` + `SRAM`; PANIC уходит под него). Донор
/// становится короче на длину региона. У ядра CM4 двухъядерных H7 и у
/// не-M7 чипов региона нет. Тест `every_m7_chip_gets_a_dma_region` ловит M7-чип,
/// которому не нашлось блока.
fn dma_nc_region(chip: &str, regions: &[RawRegion], ram_end: u64) -> Option<DmaNc> {
    if !is_cortex_m7(chip) {
        return None;
    }
    let donors = regions.iter().filter(|r| {
        r.kind == RegionKind::Ram
            && (r.name == "AXISRAM"
                || (r.name == "SRAM1" && (0x2400_0000..0x2500_0000).contains(&r.address)))
    });
    for donor in donors {
        let end = donor.address + donor.size;
        if let Some(length) = dma_nc_size(end, donor.size) {
            return Some(DmaNc {
                origin: end - length,
                length,
                donor: Some(donor.name.clone()),
            });
        }
    }
    let length = dma_nc_size(ram_end, ram_end - RAM_BASE)?;
    Some(DmaNc {
        origin: ram_end - length,
        length,
        donor: None,
    })
}

/// `SRAM4` домена D3 у H7 целиком — память BDMA (SPI6, I2C4, LPUART1, ADC3):
/// остальные блоки ей недоступны. Кэш данных её не видит так же, как DMA1/2, поэтому
/// блок тоже non-cacheable (`bsp::dma_nc`, регион MPU по границам блока).
fn bdma_nc_region(chip: &str, regions: &[RawRegion]) -> Option<(u64, u64)> {
    if !chip.starts_with("h7") || !is_cortex_m7(chip) {
        return None;
    }
    let sram4 = regions
        .iter()
        .find(|r| r.name == "SRAM4" && r.kind == RegionKind::Ram && r.address == H7_SRAM4)?;
    (sram4.size.is_power_of_two() && sram4.size >= 32 && sram4.address.is_multiple_of(sram4.size))
        .then_some((sram4.address, sram4.size))
}
/// Чип, у которого RAM меньше двух минимальных резервов, автораскладки не
/// получает вовсе (и генератор на нём падает). Среди STM32 таких нет
/// — самый скромный вариант это 2 KiB, — но инвариант «есть раскладка →
/// есть PANIC» должен держаться без оговорок.
const MIN_RAM_FOR_LAYOUT: u64 = 2 * PANIC_MIN;

/// Второй банк flash — куда кладётся `DFU` (см. `compute_ota_partitions`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Bank2 {
    origin: u64,
    length: u64,
}

/// Второй банк по именам регионов (`BANK_2`, `BANK_2_REGION_n`): непрерывная
/// цепочка от первого из них. `None` — банк один.
///
/// Банк 2, начинающийся внутри банка 1, — не банк, а ошибка метаданных: у
/// STM32U375/U385 `stm32-data` объявляет `BANK_2` с 0x0801_0000 при `BANK_1`
/// с 0x0800_0000 на 512 KiB. Положить туда DFU значило бы писать образ поверх
/// своего же кода, поэтому такой чип считается однобанковым — как и раньше,
/// когда налезающий регион просто не попадал в цепочку flash.
fn bank2_of(regions: &[RawRegion]) -> Option<Bank2> {
    let flash = || regions.iter().filter(|r| r.kind == RegionKind::Flash);
    let bank1_end = flash()
        .filter(|r| r.name.starts_with("BANK_1"))
        .map(|r| r.address + r.size)
        .max()?;
    let mut bank: Vec<&RawRegion> = flash().filter(|r| r.name.starts_with("BANK_2")).collect();
    bank.sort_by_key(|r| r.address);
    let origin = bank.first()?.address;
    if origin < bank1_end {
        return None;
    }
    let mut end = origin;
    for region in bank {
        if region.address != end {
            break;
        }
        end += region.size;
    }
    Some(Bank2 {
        origin,
        length: end - origin,
    })
}

/// Собирает карту памяти чипа: непрерывную flash-цепочку от базы, RAM-цепочку
/// от базы и — если помещается — партиции OTA. `None` только когда считать не
/// из чего (нет flash- или RAM-цепочки от базового адреса); тогда `main`
/// отказывается писать `chip-data/` (см. `without_layout`).
fn compute_memory_layout(chip: &str, regions: &[RawRegion]) -> Option<MemoryLayout> {
    let mut flash: Vec<&RawRegion> = regions
        .iter()
        .filter(|r| r.kind == RegionKind::Flash && r.flash.is_some())
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
    let bank2 = bank2_of(regions);
    // Второй банк вне цепочки — между банками разрыв (H742/743/745/747 и H7A3
    // с 1 MiB: банк 2 с 0x0810_0000; U5F7/U5F9 с 2 MiB). Приложению без OTA он
    // не достаётся (раздел кода обязан быть непрерывным), а с OTA в нём лежит
    // DFU. Стыкующийся банк 2 — просто продолжение flash.
    let bank_gap = bank2.filter(|b| b.origin >= FLASH_BASE + flash_total);
    // PAGE_SIZE в терминах embassy-boot — не наш const generic, а
    // `max(ACTIVE::ERASE_SIZE, DFU::ERASE_SIZE)` (`boot_loader.rs`). Обе
    // партиции живут поверх цельного `Flash` (`crates-cross/boot/src/main.rs`), у
    // которого `NorFlash::ERASE_SIZE = MAX_ERASE_SIZE` всего чипа — отсюда
    // максимум по цепочке (и второму банку, если он за разрывом), а не
    // erase_size конкретного региона.
    let page_size = chain
        .iter()
        .copied()
        .chain(regions.iter().filter(|r| {
            bank_gap.is_some() && r.kind == RegionKind::Flash && r.name.starts_with("BANK_2")
        }))
        .filter_map(|r| r.flash)
        .map(|f| f.erase_size)
        .max()?;
    let write_size = chain[0].flash?.write_size;
    if page_size == 0 || write_size == 0 {
        return None;
    }
    // По первому региону цепочки, как и `write_size`: у чипа стёртое
    // состояние одно на весь контроллер флеша, а не своё у каждого банка.
    let erase_zero = chain[0].flash?.erase_value == 0;

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
    if ram_total < MIN_RAM_FOR_LAYOUT {
        return None;
    }
    // Резерв отрезается от самого конца RAM — до её верхней границы; если
    // `DMA_NC` берётся из хвоста цепочки (F7), он занимает самый конец, а PANIC —
    // место под ним.
    let dma_nc = dma_nc_region(chip, regions, ram_end);
    let chain_tail = dma_nc
        .as_ref()
        .filter(|d| d.donor.is_none())
        .map_or(0, |d| d.length);
    let reserved = (ram_total / PANIC_FRACTION).clamp(PANIC_MIN, PANIC_MAX);
    let ram_length = ram_total - reserved - chain_tail;
    let panic = (RAM_BASE + ram_length, reserved);

    Some(MemoryLayout {
        flash_origin: FLASH_BASE,
        flash_length: flash_total,
        ram_origin: RAM_BASE,
        ram_length,
        panic,
        // Границы цепочек, а не ram_length: PANIC отрезан от RAM, но лежит
        // внутри той же цепочки — регионом его дублировать не надо.
        extra_regions: extra_region_lines(
            regions,
            FLASH_BASE + flash_total,
            ram_end,
            dma_nc.as_ref(),
        ),
        write_size,
        page_size,
        erase_zero,
        dfu_bank_origin: bank_gap.map(|b| b.origin),
        dma_nc,
        bdma_nc: bdma_nc_region(chip, regions),
        ota: compute_ota_partitions(
            chip,
            &chain,
            flash_total,
            page_size,
            write_size,
            false,
            bank_gap,
        ),
        config: compute_config_partition(
            chip,
            &chain,
            flash_total,
            page_size,
            write_size,
            bank_gap,
        ),
        external: compute_external_ota(chip, &chain, flash_total, page_size, false),
        external_config: compute_external_ota(chip, &chain, flash_total, page_size, true),
    })
}

/// Раскладка OTA, когда `DFU` и `STATE` уезжают на внешнюю флешь.
///
/// Внутри — bootloader и `ACTIVE`, больше ничего: `ACTIVE` начинается с первой
/// границы страницы чипа после `BOOTLOADER_TARGET` (раздел над цельным
/// `Flash`, `Partition::new` требует смещения, кратного его странице) и
/// забирает всё до конца flash, сколько ложится целыми страницами обмена.
/// Bootloader не растёт сверх нужного ради `STATE` — `STATE` снаружи.
///
/// На F2/F4/F7 зона bootloader'а всё равно 128 KiB: `ACTIVE` не может начаться
/// посреди 128-килобайтного сектора, а мелкие сектора перед ним — только
/// bootloader'у.
///
/// Снаружи — `STATE`, `DFU`, раздел настроек подряд с нуля, всё по
/// `ext_align`. Четыре проверки `assert_partitions` при этом выполнены для
/// любого драйвера, чьё стирание делит `page_size`, — см.
/// `assert_external_invariants`.
///
/// `internal_settings` — настройки во внутреннем флеше. Сначала — в две
/// соседние равные страницы зоны bootloader'а за `BOOTLOADER_TARGET`: на F4
/// это сектора 2–3 (2×16 KiB), которые иначе пропадают, и `ACTIVE` не теряет
/// ничего. Нет таких — две последние страницы flash, на них короче `ACTIVE`.
fn compute_external_ota(
    chip: &str,
    chain: &[&RawRegion],
    flash_total: u64,
    page_size: u64,
    internal_settings: bool,
) -> Result<ExternalOta, String> {
    let page = page_size.max(EXT_ERASE_MIN);
    let zone = BOOTLOADER_TARGET.next_multiple_of(page_size);
    let erase_size_at = |offset: u64| -> Option<u64> {
        let addr = FLASH_BASE + offset;
        chain
            .iter()
            .find(|r| addr >= r.address && addr < r.address + r.size)
            .and_then(|r| r.flash)
            .map(|f| f.erase_size)
    };
    let in_zone = if internal_settings {
        reserve_sectors(&erase_size_at, zone).and_then(|sectors| {
            sectors.windows(2).find_map(|pair| {
                let ((offset, erase), (_, next)) = (pair[0], pair[1]);
                (offset >= BOOTLOADER_TARGET && erase == next).then_some(SettingsPartition {
                    origin: FLASH_BASE + offset,
                    length: 2 * erase,
                    erase_size: erase,
                })
            })
        })
    } else {
        None
    };
    let tail_length = if internal_settings && in_zone.is_none() {
        if !flash_total.is_multiple_of(page_size) {
            return Err(
                "flash не кратен странице чипа: раздел настроек в хвосте не ляжет \
                        на границу сектора"
                    .to_string(),
            );
        }
        2 * page_size
    } else {
        0
    };
    let active_length = flash_total.saturating_sub(zone + tail_length) / page * page;
    if active_length == 0 {
        return Err(format!(
            "flash {} KiB: за bootloader'ом ({} KiB) не остаётся ни страницы обмена ({} KiB)",
            flash_total / 1024,
            zone / 1024,
            page / 1024
        ));
    }
    let settings = in_zone.or_else(|| {
        (tail_length > 0).then_some(SettingsPartition {
            origin: FLASH_BASE + flash_total - tail_length,
            length: tail_length,
            erase_size: page_size,
        })
    });
    let ext_align = page.max(EXT_ALIGN);
    // Журнал — на самую мелкую страницу обмена, какая может выйти: драйвер со
    // стиранием мельче `page` даст `PAGE_SIZE` = странице чипа, и страниц
    // (а с ними слов журнала) станет больше.
    let journal = (2 + 4 * (active_length / page_size)) * EXT_WRITE_MAX;
    let state_length = journal.next_multiple_of(ext_align);
    let dfu_length = (active_length + page).next_multiple_of(ext_align);
    let external = ExternalOta {
        bootloader_length: in_zone.map_or(zone, |s| s.origin - FLASH_BASE),
        active_origin: FLASH_BASE + zone,
        active_length,
        page_size: page,
        ext_align,
        state_offset: 0,
        state_length,
        dfu_offset: state_length,
        dfu_length,
        config_offset: state_length + dfu_length,
        config_length: 2 * ext_align,
        settings,
    };
    assert_external_invariants(chip, &external, page_size);
    Ok(external)
}

/// Проверки `assert_partitions` и `Partition::new` для внешней раскладки — на
/// этапе генерации, по той же причине, что `assert_embassy_boot_invariants`.
/// `chip_page` — крупнейший сектор чипа: страница цельного `Flash` под
/// `ACTIVE`.
fn assert_external_invariants(chip: &str, e: &ExternalOta, chip_page: u64) {
    let page = e.page_size;
    assert!(
        page.is_multiple_of(chip_page) && page.is_multiple_of(EXT_ERASE_MIN),
        "{chip}: страница обмена {page} не кратна странице чипа или EXT_ERASE_MIN"
    );
    assert!(
        e.ext_align.is_multiple_of(page),
        "{chip}: шаг внешних разделов {} не кратен странице обмена {page}",
        e.ext_align
    );
    assert!(
        e.active_length.is_multiple_of(page) && e.dfu_length.is_multiple_of(page),
        "{chip}: ACTIVE {} или DFU {} не кратен PAGE_SIZE {page}",
        e.active_length,
        e.dfu_length
    );
    assert!(
        e.dfu_length >= e.active_length + page,
        "{chip}: DFU {} не на страницу больше ACTIVE {}",
        e.dfu_length,
        e.active_length
    );
    let journal_words = 2 + 4 * (e.active_length / chip_page);
    assert!(
        journal_words * EXT_WRITE_MAX <= e.state_length,
        "{chip}: журналу прогресса нужно {journal_words} слов по {EXT_WRITE_MAX}, в STATE {} байт",
        e.state_length
    );
    let active_offset = e.active_origin - FLASH_BASE;
    assert!(
        active_offset.is_multiple_of(chip_page) && e.bootloader_length <= active_offset,
        "{chip}: ACTIVE не с границы страницы чипа или налезает на bootloader"
    );
    if let Some(s) = e.settings {
        let offset = s.origin - FLASH_BASE;
        let before_active = offset == e.bootloader_length && offset + s.length <= active_offset;
        let after_active = offset >= active_offset + e.active_length;
        assert!(
            s.length == 2 * s.erase_size
                && offset.is_multiple_of(s.erase_size)
                && (before_active || after_active),
            "{chip}: внутренний раздел настроек {offset:#x}+{} не на месте",
            s.length
        );
    } else {
        assert_eq!(
            e.bootloader_length, active_offset,
            "{chip}: без настроек bootloader занимает всю зону до ACTIVE"
        );
    }
    for (name, offset, length) in [
        ("STATE", e.state_offset, e.state_length),
        ("DFU", e.dfu_offset, e.dfu_length),
        ("CONFIG", e.config_offset, e.config_length),
    ] {
        assert!(
            offset.is_multiple_of(e.ext_align) && length.is_multiple_of(e.ext_align),
            "{chip}: внешний {name} (смещение {offset:#x}, размер {length}) не по шагу {}",
            e.ext_align
        );
    }
    assert!(
        e.state_offset + e.state_length <= e.dfu_offset
            && e.dfu_offset + e.dfu_length <= e.config_offset,
        "{chip}: внешние разделы накладываются"
    );
}

/// Раздел под настройки: две страницы стирания.
///
/// Две, а не одна, — требование `sequential-storage`: её `map` чередует
/// страницы, чтобы не выхаживать одну и ту же, и на одной странице просто не
/// работает («The map needs at least 2 pages to operate»). Одной и не хватило
/// бы: перед стиранием старой страницы живые записи переносятся на свободную.
///
/// Без OTA раздел — две последние страницы flash (`PAGE_SIZE`, крупнейший
/// сектор): в начале flash стоит таблица векторов приложения. С OTA есть второй
/// вариант — две соседние мелкие страницы в резерве перед `ACTIVE`, между
/// bootloader'ом и `STATE` (на F4 — сектора 2–3 по 16 KiB вместо 2×128 KiB в
/// хвосте). Берётся тот, при котором `ACTIVE` больше; при равенстве — хвост,
/// чтобы не двигать раскладку чипов с равными секторами без выигрыша.
///
/// `None` — не выделяем: либо после раздела не остаётся ни страницы под
/// приложение, либо flash не кратен странице (тогда граница раздела не легла
/// бы на сектор, а `Flash::erase` отказывает на невыровненном диапазоне).
fn compute_config_partition(
    chip: &str,
    chain: &[&RawRegion],
    flash_total: u64,
    page_size: u64,
    write_size: u64,
    bank_gap: Option<Bank2>,
) -> Option<ConfigPartition> {
    let length = page_size.checked_mul(2)?;
    if !flash_total.is_multiple_of(page_size) || flash_total < length + page_size {
        return None;
    }
    let flash_length = flash_total - length;
    let tail = SettingsPartition {
        origin: FLASH_BASE + flash_length,
        length,
        erase_size: page_size,
    };
    // Раздел в хвосте: OTA-схема считается от flash, ставшего на две страницы
    // меньше.
    let behind = compute_ota_partitions(
        chip,
        chain,
        flash_length,
        page_size,
        write_size,
        false,
        bank_gap,
    )
    .map(|partitions| OtaPartitions {
        settings: Some(tail),
        ..partitions
    });
    // Раздел в резерве перед ACTIVE: схема — от всего flash.
    let ahead = compute_ota_partitions(
        chip,
        chain,
        flash_total,
        page_size,
        write_size,
        true,
        bank_gap,
    );
    let ota = match (ahead, behind) {
        (Ok(ahead), Ok(behind)) if ahead.active_length > behind.active_length => Ok(ahead),
        (Ok(ahead), Err(_)) => Ok(ahead),
        (_, behind) => behind,
    };
    Some(ConfigPartition {
        tail,
        flash_length,
        ota,
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
fn extra_region_lines(
    regions: &[RawRegion],
    flash_end: u64,
    ram_end: u64,
    dma_nc: Option<&DmaNc>,
) -> Vec<String> {
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

    let mut lines: Vec<String> = sorted
        .iter()
        .map(|r| {
            let attrs = match r.kind {
                RegionKind::Flash => "rx",
                RegionKind::Ram => "xrw",
                // EEPROM пишется через контроллер flash, а не обычной записью,
                // поэтому без `w`: класть туда `.data` нельзя.
                RegionKind::Eeprom => "r",
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
        .collect();

    // `DMA_NC`: донор становится короче на его длину; хвост основной цепочки
    // RAM в `extra_regions` не лежит, и строка добавляется первой (адреса ниже).
    if let Some(dma) = dma_nc {
        match &dma.donor {
            Some(donor) => {
                if let Some(block) = regions.iter().find(|r| &r.name == donor)
                    && let Some(i) = lines
                        .iter()
                        .position(|line| line.trim_start().starts_with(donor.as_str()))
                {
                    lines[i] = region_line(donor, "xrw", block.address, block.size - dma.length);
                    lines.insert(i + 1, region_line("DMA_NC", "xrw", dma.origin, dma.length));
                }
            }
            None => lines.insert(0, region_line("DMA_NC", "xrw", dma.origin, dma.length)),
        }
    }
    lines
}

/// Раскладка OTA со страницей обмена, при необходимости крупнее физической.
///
/// Журнал `STATE` — `(2 + 4*N) * WRITE_SIZE` байт при N страницах обмена в
/// `ACTIVE`, и на чипе с мелкой страницей и крупным flash он съедает зону
/// bootloader'а (L0: страница 128 байт, ~640 страниц — 10 KiB из 32 KiB, и
/// подписанный `boot` не влезает). Поэтому, когда журнал при физической
/// странице `page_size` больше [`STATE_BUDGET`], страница обмена удваивается
/// (`page_size * 2^k`, не больше [`SWAP_PAGE_MAX`]) до первой, при которой он
/// укладывается; прошивка оборачивает `ACTIVE`/`DFU` в `ErasePage<_, SWAP_PAGE_SIZE>`.
/// Не укладывается и на максимуме — остаётся физическая страница, а запас под
/// код держит [`BOOTLOADER_MIN`].
///
/// Расширяется только чип с равными секторами и без `DFU` в банке 2 за
/// разрывом: на F2/F4/F7 страница обмена и так крупнейший сектор, а банк 2 за
/// разрывом лежит в другом регионе `Flash`, и единая страница на оба раздела
/// не получилась бы.
fn compute_ota_partitions(
    chip: &str,
    chain: &[&RawRegion],
    flash_total: u64,
    page_size: u64,
    write_size: u64,
    with_settings: bool,
    bank_gap: Option<Bank2>,
) -> Result<OtaPartitions, String> {
    let at = |swap| {
        compute_ota_at(
            chip,
            chain,
            flash_total,
            swap,
            write_size,
            with_settings,
            bank_gap,
        )
    };
    let fits = |p: &OtaPartitions| journal_bytes(p, write_size) <= STATE_BUDGET;
    let base = at(page_size);
    let uniform = bank_gap.is_none()
        && chain
            .iter()
            .all(|r| r.flash.is_some_and(|f| f.erase_size == page_size));
    if !uniform || base.as_ref().is_ok_and(fits) {
        return base;
    }
    let mut swap = page_size;
    while swap < SWAP_PAGE_MAX {
        swap *= 2;
        if let Ok(partitions) = at(swap)
            && fits(&partitions)
        {
            return Ok(partitions);
        }
    }
    base
}

/// Крупнейший сектор стирания по цепочке flash: страница цельного `Flash`.
fn chain_page(chain: &[&RawRegion]) -> u64 {
    chain
        .iter()
        .filter_map(|r| r.flash)
        .map(|f| f.erase_size)
        .max()
        .unwrap_or(1)
}

/// Журнал обмена `embassy-boot` в байтах: слово записи на страницу `ACTIVE` в
/// каждом из четырёх проходов плюс два служебных (проверка 4).
fn journal_bytes(p: &OtaPartitions, write_size: u64) -> u64 {
    (2 + 4 * (p.active_length / p.swap_page_size)) * write_size
}

/// Подбирает `BOOTLOADER`/`BOOTLOADER_STATE`/`ACTIVE`/`DFU` для страницы обмена
/// `page_size` (физической или расширенной, см. [`compute_ota_partitions`]; вся
/// арифметика ниже — в её единицах) под все четыре
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
/// Но есть пятая проверка, не из `assert_partitions`: раздел строится
/// `Partition::new` из `embassy-embedded-hal`, а тот паникует, если смещение
/// или размер не кратны `NorFlash::ERASE_SIZE` флеша под разделом. У цельного
/// `Flash` это самый крупный сектор, и на F2/F4/F7 `STATE` в мелком секторе
/// (0x10000 при странице 128 KiB у F4) ронял и bootloader, и приложение на
/// первом же старте — у всех 223 таких чипов с OTA. Поэтому `STATE` строится
/// поверх `adapters::flash::ErasePage` со страницей своего сектора, и размер этого
/// сектора — `bootloader_state_erase_size` — уходит в шаблон вместе с адресами.
/// Сам сектор — самый мелкий в резерве, см. `place_state`.
///
/// Обычно `DFU` идёт сразу за `ACTIVE`, а остаток flash делится между ними
/// пополам — так прошивка выходит наибольшей. Банки, которые стыкуются, для
/// этого — один непрерывный flash. Если же банк 2 стоит за разрывом
/// (`bank_gap`; `flash_total` тогда кончается на банке 1), кода в нём быть не
/// может, и он целиком отдаётся под `DFU`: `ACTIVE` — всё, что осталось от
/// банка 1 после резерва (но не больше, чем позволяет банк 2), `DFU` — с
/// начала банка 2. Пишет его прошивка через регион банка
/// (`MemoryLayout::dfu_bank_origin`).
///
/// `Err` — схема не помещается; текст объясняет, чего именно не хватило, и
/// доходит до пользователя при генерации.
fn compute_ota_at(
    chip: &str,
    chain: &[&RawRegion],
    flash_total: u64,
    page_size: u64,
    write_size: u64,
    with_settings: bool,
    bank_gap: Option<Bank2>,
) -> Result<OtaPartitions, String> {
    // Банк 2 за разрывом — весь под `DFU`.
    let dfu_zone = bank_gap.map(|b| (b.origin, b.origin + b.length));
    let erase_size_at = |offset_from_flash_base: u64| -> Option<u64> {
        let addr = FLASH_BASE + offset_from_flash_base;
        chain
            .iter()
            .find(|r| addr >= r.address && addr < r.address + r.size)
            .and_then(|r| r.flash)
            .map(|f| f.erase_size)
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
    let max_extra_pages =
        MAX_EXTRA_RESERVED_PAGES.max(MAX_EXTRA_RESERVED_BYTES.div_ceil(page_size));
    let mut reserved_pages = initial_pages;
    loop {
        if reserved_pages > initial_pages + max_extra_pages {
            return Err(format!(
                "резерв под bootloader не подобрался, дорастив его на {max_extra_pages} страниц по {}",
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

        let active_room = (flash_total - reserved_size) / page_size;
        let (active_pages, dfu_pages, dfu_origin) = if let Some((origin, end)) = dfu_zone {
            // DFU обязан быть на страницу больше ACTIVE (проверка 3), так что
            // ACTIVE упирается в меньшее из двух: остаток банка 1 и банк 2 без
            // страницы.
            let dfu_room = (end - origin) / page_size;
            let active_pages = active_room.min(dfu_room.saturating_sub(1));
            if active_pages == 0 {
                return Err(format!(
                    "двухбанковый flash при секторе {}: под BOOTLOADER+STATE уходит {}, \
                     в банке 1 под ACTIVE остаётся страниц: {active_room}, в банке 2 под DFU — \
                     {dfu_room} (нужно хотя бы 1 и 2)",
                    size(page_size),
                    size(reserved_size)
                ));
            }
            (active_pages, active_pages + 1, origin)
        } else {
            let remaining_pages = active_room;
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
            let dfu_start = FLASH_BASE + reserved_size + active_pages * page_size;
            (active_pages, remaining_pages - active_pages, dfu_start)
        };

        // Проверка 4, решённая относительно STATE: сколько журнала нужно под
        // такой ACTIVE.
        let state_needed = (2 + 4 * active_pages) * write_size;
        let placed = reserve_sectors(&erase_size_at, reserved_size).and_then(|sectors| {
            if with_settings {
                place_settings_and_state(&sectors, state_needed)
                    .map(|(settings, state)| (Some(settings), state))
            } else {
                place_state(&sectors, state_needed, BOOTLOADER_MIN).map(|state| (None, state))
            }
        });
        let Some((settings, (state_offset, state_length, state_erase))) = placed else {
            // Журнал (и раздел настроек) не легли в сектора резерва, не отняв у
            // bootloader'а минимума, — помогает резерв крупнее: в нём больше
            // секторов, а ACTIVE, а с ним и журнал, меньше.
            reserved_pages += 1;
            continue;
        };

        let active_origin = FLASH_BASE + reserved_size;
        let active_length = active_pages * page_size;
        let partitions = OtaPartitions {
            bootloader_length: settings.map_or(state_offset, |s| s.origin - FLASH_BASE),
            bootloader_state_origin: FLASH_BASE + state_offset,
            bootloader_state_length: state_length,
            bootloader_state_erase_size: state_erase,
            swap_page_size: page_size,
            active_origin,
            active_length,
            dfu_origin,
            dfu_length: dfu_pages * page_size,
            settings,
        };
        assert_embassy_boot_invariants(chip, &partitions, chain_page(chain), write_size);
        return Ok(partitions);
    }
}

/// Где в резерве `[0, reserved_size)` лежит `BOOTLOADER_STATE`: смещение,
/// длина и размер его сектора. `None` — не лёг.
///
/// В самых мелких секторах резерва и как можно ближе к его концу. Журналу
/// нужны десятки байт, а стирается раздел при каждом обновлении, так что
/// мелкий сектор и места не отнимает, и стирается быстрее. Всё, что перед
/// ним, — bootloader, и он обязан получить хотя бы `BOOTLOADER_MIN`. Что
/// остаётся между `STATE` и `ACTIVE`, не занято: на F4 это 64-килобайтный
/// сектор 4 — под `ACTIVE` он не годится (страница там 128 KiB), а `STATE`
/// ему мельче предпочтителен.
///
/// На чипе с равными секторами «самые мелкие» — все, «ближе к концу» — вплотную
/// к `ACTIVE`, то есть раскладка та же, что до этого правила.
///
/// Если мелкие сектора стоят так близко к началу, что bootloader'у перед ними
/// не хватает минимума, пробуется следующий размер — а не сразу резерв крупнее:
/// тот не поменял бы, где лежат мелкие сектора.
///
/// `sectors` — `(смещение, размер)` секторов, где можно искать; `min_offset` —
/// раньше какого смещения `STATE` начинаться не может (перед ним bootloader или
/// раздел настроек).
fn place_state(
    sectors: &[(u64, u64)],
    state_needed: u64,
    min_offset: u64,
) -> Option<(u64, u64, u64)> {
    let sizes: BTreeSet<u64> = sectors.iter().map(|(_, erase)| *erase).collect();
    sizes.into_iter().find_map(|size| {
        let count = usize::try_from(state_needed.div_ceil(size)).ok()?;
        // Окно из `count` подряд идущих секторов этого размера — от последнего
        // к первому.
        sectors
            .windows(count)
            .rev()
            .find(|window| window.iter().all(|(_, erase)| *erase == size))
            .map(|window| (window[0].0, size * count as u64, size))
            .filter(|(state_offset, _, _)| *state_offset >= min_offset)
    })
}

/// Сектора резерва `[0, reserved_size)` по порядку: `(смещение, размер)`.
/// `None` — на каком-то смещении размер сектора неизвестен.
fn reserve_sectors(
    erase_size_at: &impl Fn(u64) -> Option<u64>,
    reserved_size: u64,
) -> Option<Vec<(u64, u64)>> {
    let mut sectors = Vec::new();
    let mut offset = 0;
    while offset < reserved_size {
        let erase = erase_size_at(offset).filter(|erase| *erase > 0)?;
        sectors.push((offset, erase));
        offset += erase;
    }
    Some(sectors)
}

/// Раздел настроек и `STATE` в резерве: bootloader, за ним настройки — две
/// соседние страницы одного размера, — за ними `STATE`. `None` — не легли.
///
/// Настройки — как можно ближе к концу резерва, но так, чтобы после них нашлось
/// место под `STATE`: на F4 это сектора 2–3 (2×16 KiB), `STATE` — сектор 4
/// (64 KiB), bootloader — 32 KiB. `STATE` здесь, в отличие от проекта без
/// настроек, может попасть в сектор крупнее: журналу нужны десятки байт и
/// размер сектора ему всё равно, а настройкам нужны две равные страницы, и
/// мелкие сектора отдаются им.
///
/// Bootloader перед разделом получает не меньше `BOOTLOADER_TARGET`, а не
/// `BOOTLOADER_MIN`, как без настроек: раздел в резерве — это выгода для
/// `ACTIVE`, и оплачивать её бинарником bootloader'а нельзя. Иначе на чипах с
/// равными секторами раздел садился бы в запас bootloader'а и ужимал его до
/// 16 KiB ради одной-двух страниц `ACTIVE`; с этой границей такие чипы честно
/// получают раздел в хвосте (см. `compute_config_partition`).
fn place_settings_and_state(
    sectors: &[(u64, u64)],
    state_needed: u64,
) -> Option<(SettingsPartition, (u64, u64, u64))> {
    (0..sectors.len().saturating_sub(1)).rev().find_map(|i| {
        let (offset, erase) = sectors[i];
        if sectors[i + 1].1 != erase || offset < BOOTLOADER_TARGET {
            return None;
        }
        let state = place_state(&sectors[i + 2..], state_needed, 0)?;
        Some((
            SettingsPartition {
                origin: FLASH_BASE + offset,
                length: 2 * erase,
                erase_size: erase,
            },
            state,
        ))
    })
}

/// Те же четыре проверки, что `assert_partitions` делает в рантайме на плате, и
/// пятая — от `Partition::new` (кратность смещения и размера раздела странице
/// флеша под ним), — но здесь, на этапе генерации. Страница обмена —
/// `p.swap_page_size`: кратна `chip_page` (крупнейший сектор чипа), и по ней
/// же сверяются `ACTIVE`/`DFU` (при расширенной странице прошивка строит их
/// поверх `ErasePage` с этой страницей). Паника вместо `Err`: `compute_ota_partitions`
/// обязан выдавать только валидные раскладки, невалидная — баг в нём самом, а
/// не «чип не подошёл». Ровно этой проверки не хватало: 279 из 972 прежних
/// раскладок нарушали четвёртую и роняли bootloader на первом же старте.
fn assert_embassy_boot_invariants(chip: &str, p: &OtaPartitions, chip_page: u64, write_size: u64) {
    let page_size = p.swap_page_size;
    assert!(
        page_size.is_multiple_of(chip_page),
        "{chip}: страница обмена {page_size} не кратна странице чипа {chip_page}"
    );
    assert!(
        p.active_length.is_multiple_of(page_size),
        "{chip}: ACTIVE {} не кратен PAGE_SIZE {page_size}",
        p.active_length
    );
    assert!(
        p.dfu_length.is_multiple_of(page_size),
        "{chip}: DFU {} не кратен PAGE_SIZE {page_size}",
        p.dfu_length
    );
    assert!(
        p.dfu_length >= p.active_length + page_size,
        "{chip}: DFU {} не на страницу больше ACTIVE {}",
        p.dfu_length,
        p.active_length
    );
    let journal_words = 2 + 4 * (p.active_length / page_size);
    let state_words = p.bootloader_state_length / write_size;
    assert!(
        journal_words <= state_words,
        "{chip}: журналу прогресса нужно {journal_words} слов, в BOOTLOADER_STATE помещается {state_words}"
    );
    // Порядок: bootloader, [настройки,] STATE, ACTIVE, DFU — и настройки в
    // хвосте, если не в резерве. Между STATE и ACTIVE может остаться незанятый
    // сектор (см. `place_state`), между ACTIVE и DFU — остаток банка 1 у
    // двухбанкового чипа; хвостовые настройки лежат после ACTIVE и не задевают
    // DFU (за ним — на непрерывном flash, перед ним — у банка 2 за разрывом).
    let ahead = p.settings.filter(|s| s.origin < p.active_origin);
    let after_bootloader = ahead.map_or(p.bootloader_state_origin, |s| s.origin);
    let active_end = p.active_origin + p.active_length;
    let dfu_end = p.dfu_origin + p.dfu_length;
    assert!(
        after_bootloader == FLASH_BASE + p.bootloader_length
            && ahead.is_none_or(|s| s.origin + s.length <= p.bootloader_state_origin)
            && p.bootloader_state_origin + p.bootloader_state_length <= p.active_origin
            && p.dfu_origin >= active_end
            && p.settings.is_none_or(|s| {
                ahead.is_some()
                    || (s.origin >= active_end
                        && (s.origin + s.length <= p.dfu_origin || s.origin >= dfu_end))
            }),
        "{chip}: партиции накладываются или идут не по порядку"
    );
    if let Some(s) = p.settings {
        assert!(
            s.length == 2 * s.erase_size,
            "{chip}: раздел настроек {} — не две страницы по {}",
            s.length,
            s.erase_size
        );
    }
    // `ACTIVE`/`DFU` строятся поверх цельного `Flash` либо `ErasePage` со страницей
    // обмена, `STATE` — поверх `ErasePage` со страницей своего сектора.
    let state_offset = p.bootloader_state_origin - FLASH_BASE;
    for (name, offset, length, page) in [
        (
            "ACTIVE",
            p.active_origin - FLASH_BASE,
            p.active_length,
            page_size,
        ),
        ("DFU", p.dfu_origin - FLASH_BASE, p.dfu_length, page_size),
        (
            "BOOTLOADER_STATE",
            state_offset,
            p.bootloader_state_length,
            p.bootloader_state_erase_size,
        ),
    ]
    .into_iter()
    .chain(
        p.settings
            .map(|s| ("CONFIG", s.origin - FLASH_BASE, s.length, s.erase_size)),
    ) {
        assert!(
            offset.is_multiple_of(page) && length.is_multiple_of(page),
            "{chip}: {name} (смещение {offset:#x}, размер {length}) не кратен своей странице {page}: Partition::new запаникует"
        );
    }
}

fn format_addr(v: u64) -> String {
    format!("0x{v:08X}")
}

/// Число как литерал Rust с разделителями разрядов (`131_072`): значение
/// уходит прямо в `const` crates-cross (`STATE_ERASE_SIZE`), а там
/// `clippy::unreadable_literal` под `-D warnings` не пропускает шесть цифр
/// подряд.
fn rust_literal(v: u64) -> String {
    let digits = v.to_string();
    let mut out = String::new();
    for (i, c) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push('_');
        }
        out.push(c);
    }
    out
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

/// Раскладки по файлам семейств: ключ — [`layout_family`], значение — текст
/// `chip-data/layout/<ключ>.rhai`. Одной таблицей это 89 тысяч строк, а хуку
/// нужна одна запись — он импортирует только файл своего семейства.
fn format_memory_layouts(layouts: &BTreeMap<&str, MemoryLayout>) -> BTreeMap<String, String> {
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    for (suffix, m) in layouts {
        let mut out = String::new();
        out.push_str(&format!("    \"{suffix}\": #{{\n"));
        push_field(&mut out, "flash_origin", &format_addr(m.flash_origin));
        push_field(&mut out, "flash_length", &format_size(m.flash_length));
        push_field(&mut out, "ram_origin", &format_addr(m.ram_origin));
        push_field(&mut out, "ram_length", &format_size(m.ram_length));
        let (panic_origin, panic_length) = m.panic;
        push_field(&mut out, "panic_origin", &format_addr(panic_origin));
        push_field(&mut out, "panic_length", &format_size(panic_length));
        if !m.extra_regions.is_empty() {
            out.push_str("        \"extra_regions\": [\n");
            for line in &m.extra_regions {
                assert!(!line.contains('"'), "кавычка в строке региона: {line}");
                out.push_str(&format!("            \"{line}\",\n"));
            }
            out.push_str("        ],\n");
        }
        push_field(&mut out, "write_size", &m.write_size.to_string());
        push_field(&mut out, "page_size", &m.page_size.to_string());
        // Строкой, а не булевым: `variable::get` в rhai-хуках читает только
        // строки, и весь остальной обмен с шаблоном устроен так же.
        push_field(
            &mut out,
            "erase_zero",
            if m.erase_zero { "true" } else { "false" },
        );
        // Только у чипов с разрывом между банками — по наличию ключа хук и
        // решает, что DFU пишется через регион банка.
        if let Some(dma) = &m.dma_nc {
            push_field(&mut out, "dma_nc_origin", &format_addr(dma.origin));
            push_field(&mut out, "dma_nc_length", &format_size(dma.length));
        }
        if let Some((origin, length)) = m.bdma_nc {
            push_field(&mut out, "bdma_nc_origin", &format_addr(origin));
            push_field(&mut out, "bdma_nc_length", &format_size(length));
        }
        if let Some(origin) = m.dfu_bank_origin {
            push_field(&mut out, "dfu_bank_origin", &format_addr(origin));
        }
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
                push_field(
                    &mut out,
                    "bootloader_state_erase_size",
                    &rust_literal(p.bootloader_state_erase_size),
                );
                push_field(&mut out, "swap_page_size", &p.swap_page_size.to_string());
                push_field(
                    &mut out,
                    "swap_paged",
                    if p.swap_page_size > m.page_size {
                        "true"
                    } else {
                        "false"
                    },
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
        // Второй набор цифр — для проекта с разделом настроек. Ключи с
        // префиксом `config_`, потому что от размера flash зависит вся
        // OTA-схема, а не только сам раздел (см. `ConfigPartition`).
        match &m.config {
            Some(config) => {
                push_field(&mut out, "config", "true");
                // Раздел в хвосте — для проекта без OTA. С OTA он может лежать
                // в другом месте: `config_ota_settings_*` ниже.
                push_field(&mut out, "config_origin", &format_addr(config.tail.origin));
                push_field(&mut out, "config_length", &format_size(config.tail.length));
                push_field(
                    &mut out,
                    "config_erase_size",
                    &rust_literal(config.tail.erase_size),
                );
                push_field(
                    &mut out,
                    "config_flash_length",
                    &format_size(config.flash_length),
                );
                match &config.ota {
                    Ok(p) => {
                        push_field(&mut out, "config_ota", "true");
                        push_field(
                            &mut out,
                            "config_bootloader_length",
                            &format_size(p.bootloader_length),
                        );
                        push_field(
                            &mut out,
                            "config_bootloader_state_origin",
                            &format_addr(p.bootloader_state_origin),
                        );
                        push_field(
                            &mut out,
                            "config_bootloader_state_length",
                            &format_size(p.bootloader_state_length),
                        );
                        push_field(
                            &mut out,
                            "config_bootloader_state_erase_size",
                            &rust_literal(p.bootloader_state_erase_size),
                        );
                        push_field(
                            &mut out,
                            "config_swap_page_size",
                            &p.swap_page_size.to_string(),
                        );
                        push_field(
                            &mut out,
                            "config_swap_paged",
                            if p.swap_page_size > m.page_size {
                                "true"
                            } else {
                                "false"
                            },
                        );
                        push_field(
                            &mut out,
                            "config_active_origin",
                            &format_addr(p.active_origin),
                        );
                        push_field(
                            &mut out,
                            "config_active_length",
                            &format_size(p.active_length),
                        );
                        push_field(&mut out, "config_dfu_origin", &format_addr(p.dfu_origin));
                        push_field(&mut out, "config_dfu_length", &format_size(p.dfu_length));
                        let settings = p
                            .settings
                            .expect("схема набора config_ всегда считается с разделом настроек");
                        push_field(
                            &mut out,
                            "config_ota_settings_origin",
                            &format_addr(settings.origin),
                        );
                        push_field(
                            &mut out,
                            "config_ota_settings_length",
                            &format_size(settings.length),
                        );
                        push_field(
                            &mut out,
                            "config_ota_settings_erase_size",
                            &rust_literal(settings.erase_size),
                        );
                    }
                    Err(note) => {
                        push_field(&mut out, "config_ota", "false");
                        push_field(&mut out, "config_note", note);
                    }
                }
            }
            None => push_field(&mut out, "config", "false"),
        }
        // Третий и четвёртый наборы — OTA с `DFU`/`STATE` на внешней флеши
        // (`ExternalOta`): настройки снаружи или их нет (`ext_`) и настройки
        // во внутреннем флеше (`ext_cfg_`).
        push_external(&mut out, "ext_", &m.external);
        push_external(&mut out, "ext_cfg_", &m.external_config);
        out.push_str("    },\n");
        files
            .entry(layout_family(suffix).to_owned())
            .or_insert_with(|| format!("{DATA_HEADER}export const MEMORY_LAYOUT = #{{\n"))
            .push_str(&out);
    }
    for body in files.values_mut() {
        body.push_str("};\n");
    }
    files
}

/// Файл раскладок, в котором лежит чип: первые два символа суффикса (`f4`,
/// `h7`, `wb`). Тот же ключ хук вычисляет в `import` — менять только вместе.
fn layout_family(suffix: &str) -> &str {
    &suffix[..2]
}

/// Поля одного набора `ExternalOta` с префиксом `prefix`. Внешние значения —
/// смещения от начала внешней флеши, а не адреса.
fn push_external(out: &mut String, prefix: &str, external: &Result<ExternalOta, String>) {
    let mut field = |key: &str, value: &str| push_field(out, &format!("{prefix}{key}"), value);
    match external {
        Ok(e) => {
            field("ota", "true");
            field("bootloader_length", &format_size(e.bootloader_length));
            field("active_origin", &format_addr(e.active_origin));
            field("active_length", &format_size(e.active_length));
            field("page_size", &rust_literal(e.page_size));
            field("align", &rust_literal(e.ext_align));
            field("state_offset", &format_addr(e.state_offset));
            field("state_length", &format_size(e.state_length));
            field("dfu_offset", &format_addr(e.dfu_offset));
            field("dfu_length", &format_size(e.dfu_length));
            field("config_offset", &format_addr(e.config_offset));
            field("config_length", &format_size(e.config_length));
            if let Some(s) = e.settings {
                field("settings_origin", &format_addr(s.origin));
                field("settings_length", &format_size(s.length));
                field("settings_erase_size", &rust_literal(s.erase_size));
            }
        }
        Err(note) => {
            field("ota", "false");
            field("note", note);
        }
    }
}

/// Только чипы, у которых блок сторожевого таймера зовётся НЕ `IWDG` — для
/// остальных хук подставляет [`DEFAULT_WATCHDOG`]. Полная таблица на 1533
/// чипа повторяла бы одно и то же слово полутора тысячами строк.
fn format_watchdogs(watchdogs: &BTreeMap<&str, String>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    out.push_str(&format!(
        "// {} шт., остальным — {DEFAULT_WATCHDOG}\n",
        watchdogs.len()
    ));
    out.push_str("export const WATCHDOG = #{\n");
    for (suffix, name) in watchdogs {
        out.push_str(&format!("    \"{suffix}\": \"{name}\",\n"));
    }
    out.push_str("};\n");
    out
}

/// RNG каждого чипа, у которого он подключается (см. [`rng_clock`]):
/// прерывание, способ дать клок и вариант мультиплексора.
fn format_rngs(chips: &BTreeMap<&str, (String, RngClock)>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    out.push_str(&format!(
        "// {} шт.: [прерывание, клок, источник]\n",
        chips.len()
    ));
    out.push_str("export const RNG = #{\n");
    for (suffix, (interrupt, (clock, source))) in chips {
        out.push_str(&format!(
            "    \"{suffix}\": [\"{interrupt}\", \"{clock}\", \"{source}\"],\n"
        ));
    }
    out.push_str("};\n");
    out
}

/// Интерфейсы связи каждого чипа (см. [`parse_interfaces`]): по ним хук
/// проверяет ответ на вопрос об интерфейсе, а `bsp` выбирает драйвер.
fn format_interfaces(chips: &BTreeMap<&str, Vec<&'static str>>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    out.push_str(&format!(
        "// {} шт.; метки: {}
",
        chips.len(),
        INTERFACE_TAGS.join(", ")
    ));
    out.push_str(
        "export const INTERFACES = #{
",
    );
    for (suffix, tags) in chips {
        let tags: Vec<String> = tags.iter().map(|t| format!("\"{t}\"")).collect();
        out.push_str(&format!(
            "    \"{suffix}\": [{}],
",
            tags.join(", ")
        ));
    }
    out.push_str(
        "};
",
    );
    out
}

/// Имена блоков интерфейсов каждого чипа (см. [`parse_instances`]): по ним
/// `resources.rs` кладёт блок выбранного интерфейса в свою группу.
fn format_instances(chips: &BTreeMap<&str, BTreeMap<&'static str, Vec<String>>>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    out.push_str(&format!(
        "// {} шт.; метка интерфейса: имена блоков в порядке выбора
",
        chips.len()
    ));
    out.push_str(
        "export const INSTANCES = #{
",
    );
    for (suffix, names) in chips {
        let names: Vec<String> = names
            .iter()
            .map(|(tag, blocks)| {
                let blocks: Vec<String> = blocks.iter().map(|b| format!("\"{b}\"")).collect();
                format!("\"{tag}\": [{}]", blocks.join(", "))
            })
            .collect();
        out.push_str(&format!(
            "    \"{suffix}\": #{{{}}},
",
            names.join(", ")
        ));
    }
    out.push_str(
        "};
",
    );
    out
}

/// Чипы с option byte `CPU_FREQ_BOOST` (см. [`SharedMetadata::cpu_freq_boost`])
/// — массивом, а не таблицей: значение у всех одно, важна только
/// принадлежность.
fn format_cpu_freq_boost(chips: &[&str]) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    out.push_str(&format!("// {} шт.\n", chips.len()));
    out.push_str("export const CPU_FREQ_BOOST = [\n");
    for suffix in chips {
        out.push_str(&format!("    \"{suffix}\",\n"));
    }
    out.push_str("];\n");
    out
}

/// Кэши чипов (см. [`chip_caches`]) — теги через пробел, как их читает Liquid
/// (`cache contains "icen"`); чипам без кэша записи нет.
fn format_caches(chips: &BTreeMap<&str, Vec<&'static str>>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    out.push_str(&format!(
        "// {} шт.
",
        chips.len()
    ));
    out.push_str(
        "export const CACHE = #{
",
    );
    for (suffix, tokens) in chips {
        out.push_str(&format!(
            "    \"{suffix}\": \"{}\",
",
            tokens.join(" ")
        ));
    }
    out.push_str(
        "};
",
    );
    out
}

/// Какой бит `FLASH_OPTR` у чипа включает двухбанковый режим.
///
/// У G4 — `DBANK` (22), у G0 — `DUAL_BANK` (21) на любом объёме. У L4+ и L5
/// бит зависит от объёма: у чипа с наибольшим flash в серии — `DBANK` (22), у
/// остальных — `DB1M`/`DB256` (21), а у старшего объёма бит 21 не определён
/// (так их различает OpenOCD, `stm32l4x.c`: `is_max_flash_size`). Серия — три
/// первых символа суффикса (`l4r`, `l4p`, `l55`): у L4R/L4S наибольший объём
/// 2 MiB, у L4P/L4Q — 1 MiB, у L55/L56 — 512 KiB, и ровно столько находит
/// максимум по чипам серии.
fn bank_switch_bits<'a>(
    variants: &BTreeMap<&'a str, &'static str>,
    layouts: &BTreeMap<&str, MemoryLayout>,
) -> BTreeMap<&'a str, (&'static str, u8)> {
    let flash = |suffix: &str| layouts.get(suffix).map_or(0, |m| m.flash_length);
    let mut series_max: BTreeMap<&str, u64> = BTreeMap::new();
    for suffix in variants.keys() {
        let max = series_max.entry(&suffix[..3]).or_default();
        *max = (*max).max(flash(suffix));
    }
    variants
        .iter()
        .map(|(suffix, variant)| {
            let bit = match *variant {
                "g4" => 22,
                "g0" => 21,
                _ if flash(suffix) == series_max[&suffix[..3]] => 22,
                _ => 21,
            };
            (*suffix, (*variant, bit))
        })
        .collect()
}

/// Чипы, которым прошивка переключает флеш в одобанковый режим, — с вариантом
/// регистров (см. [`FACTORY_DUAL_BANK`]) и битом режима через двоеточие.
fn format_bank_switch(chips: &BTreeMap<&str, (&'static str, u8)>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    out.push_str(&format!("// {} шт.\n", chips.len()));
    out.push_str("export const BANK_SWITCH = #{\n");
    for (suffix, (variant, bit)) in chips {
        out.push_str(&format!("    \"{suffix}\": \"{variant}:{bit}\",\n"));
    }
    out.push_str("};\n");
    out
}

/// Только чипы, у которых `embassy-stm32` требует выбора банковой схемы
/// (несколько карт памяти) — для остальных фичи нет вовсе, и хук подставляет
/// пустую строку.
fn format_bank_modes(modes: &BTreeMap<&str, &'static str>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    out.push_str(&format!("// {} шт.\n", modes.len()));
    out.push_str("export const BANK_MODE = #{\n");
    for (suffix, mode) in modes {
        out.push_str(&format!("    \"{suffix}\": \"{mode}\",\n"));
    }
    out.push_str("};\n");
    out
}

/// Множество идентификаторов чипов, которые знает локально установленный
/// `probe-rs` (`probe-rs chip list`), например `STM32F407VE` — без суффикса
/// корпус/темп.диапазон (`Tx`/`Hx`/...), см. обоснование в CLAUDE.md.
/// Версия `embassy-stm32` так, как она объявлена в `crates-cross/Cargo.toml` — не
/// разрешённая cargo: сверять надо именно объявление, иначе безобидный
/// патч-релиз в реестре ронял бы тест штампа.
fn declared_embassy_version(repo_root: &Path) -> anyhow::Result<String> {
    let manifest_path = repo_root.join("crates-cross").join("Cargo.toml");
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
        "{SOURCE_STAMP_PREFIX}{embassy_version} (crates-cross/Cargo.toml), probe-rs {probe_rs_version}\n"
    )
}

fn format_chip_list(suffixes: &[&str], stamp: &str) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    out.push_str(&format!("// {} шт.\n", suffixes.len()));
    out.push_str(stamp);
    out.push_str("export const CHIPS = [\n");
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
    out.push_str(DATA_HEADER);
    out.push_str(&format!("// {} шт.\n", choices.len()));
    out.push_str("export const PACKAGE_CHOICES = #{\n");
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

/// Перезаписывает каталог `dir` ровно набором `files` (путь относительно
/// `dir`, текст).
///
/// Файлы одного поколения: новый список чипов при старой раскладке памяти —
/// ровно то состояние, в котором всё выглядит правдоподобно и ничего не
/// работает. Поэтому всё пишется в соседний `<dir>.tmp` и подменяет `dir`
/// двумя переименованиями каталогов; сбой посреди записи оставляет старый
/// `dir` нетронутым. Между переименованиями `dir` на мгновение нет вовсе, и
/// упавший там прогон хук заметит громко (`import` не найдёт модуль), а не
/// молча. Каталог пишется целиком ещё и затем, чтобы не оставалось файлов
/// семейств, которых больше нет.
fn write_data_dir(dir: &Path, files: &[(String, String)]) -> anyhow::Result<()> {
    let with_suffix = |suffix: &str| {
        let mut name = dir.as_os_str().to_owned();
        name.push(suffix);
        PathBuf::from(name)
    };
    let tmp = with_suffix(".tmp");
    let old = with_suffix(".old");
    for stale in [&tmp, &old] {
        if stale.exists() {
            fs::remove_dir_all(stale)
                .with_context(|| format!("не удалось удалить {}", stale.display()))?;
        }
    }

    for (relative, body) in files {
        let path = tmp.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("не удалось создать {}", parent.display()))?;
        }
        fs::write(&path, body)
            .with_context(|| format!("не удалось записать {}", path.display()))?;
    }

    if dir.exists() {
        fs::rename(dir, &old).with_context(|| {
            format!(
                "не удалось переименовать {} в {}",
                dir.display(),
                old.display()
            )
        })?;
    }
    if let Err(err) = fs::rename(&tmp, dir) {
        // Старое поколение возвращается на место: иначе оно осталось бы только
        // в `.old`, который следующий прогон первым же делом удаляет.
        let restored = old.exists() && fs::rename(&old, dir).is_ok();
        return Err(anyhow::Error::new(err).context(format!(
            "не удалось переименовать {} в {} (старый каталог {})",
            tmp.display(),
            dir.display(),
            if restored {
                "возвращён на место".to_owned()
            } else {
                format!("остался в {}", old.display())
            },
        )));
    }
    // Новое поколение уже на месте — недоудалённый `.old` не ошибка прогона,
    // его снесёт следующий.
    if old.exists()
        && let Err(err) = fs::remove_dir_all(&old)
    {
        eprintln!("не удалось удалить {}: {err}", old.display());
    }
    Ok(())
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

    /// Порядок полей в `MemoryRegion` держится на том, что `stm32-data` печатает
    /// `size` раньше `settings`. Переедем `settings` выше — и `find("size: ")`
    /// начнёт матчить `erase_size`, вернёт `2048` вместо размера региона, и
    /// `flash_total` окажется неверным у сотен чипов, полностью молча: `parse`
    /// такой результат принимает. Разбор полей идёт поэтому по границе поля, а
    /// не по подстроке, и порядок полей больше не важен.
    #[test]
    fn a_field_is_found_even_when_a_longer_name_comes_first() {
        let block = "\
 name: \"BANK_1\",
 kind: MemoryRegionKind::Flash,
 settings: Some(FlashSettings { erase_size: 2048, write_size: 8, erase_value: 255 }),
 address: 0x8000000,
 size: 131072,
";
        assert_eq!(super::field(block, "size"), Some("131072"));
        assert_eq!(super::field(block, "erase_size"), Some("2048"));
        assert_eq!(super::field(block, "write_size"), Some("8"));
        assert_eq!(super::field(block, "address"), Some("0x8000000"));
    }

    /// Неизвестный вид региона — ошибка, а не «прочее». Иначе переименование
    /// варианта в `stm32-data` молча оставило бы все чипы без `MEMORY_LAYOUT`,
    /// а `cargo generate` закончил бы работу с нулевым кодом.
    #[test]
    fn an_unknown_region_kind_is_an_error() {
        let block = "\
 name: \"BANK_1\",
 kind: MemoryRegionKind::FlashBank1,
 address: 0x8000000,
 size: 131072,
";
        let err = region_kind(block).expect_err("неизвестный вид региона обязан быть ошибкой");
        assert!(err.to_string().contains("FlashBank1"), "{err:#}");

        // Суффикс совпадает с известным видом, а вид другой: не должен стать RAM.
        let err =
            region_kind("kind: MemoryRegionKind::BackupRam,").expect_err("BackupRam — не Ram");
        assert!(err.to_string().contains("BackupRam"), "{err:#}");

        // Известные виды по-прежнему разбираются: имя типа печатает
        // `stm32-data`, а не шаблон.
        for (printed, expected) in [
            ("MemoryRegionKind::Flash", RegionKind::Flash),
            ("MemoryRegionKind::Ram", RegionKind::Ram),
            ("MemoryRegionKind::Eeprom", RegionKind::Eeprom),
        ] {
            let block = format!("kind: {printed},");
            assert_eq!(region_kind(&block).expect("известный вид"), expected);
        }
    }

    /// Первая строка `<chip>/metadata.rs` — это `include!` общего файла, где
    /// и лежит перечень блоков. Разберись мы её неверно, сторож у ВСЕХ чипов
    /// молча стал бы дефолтным `IWDG`, включая H7, где такого блока нет.
    #[test]
    fn included_metadata_file_is_taken_from_the_include_line() {
        assert_eq!(
            parse_included_metadata("include!(\"../metadata_0180.rs\");\nuse crate::x;\n"),
            Some("../metadata_0180.rs")
        );
        assert_eq!(parse_included_metadata("pub static METADATA"), None);
    }

    /// Версия регистров — у блока нужного вида, а не у первого попавшегося.
    #[test]
    fn registers_version_is_taken_from_the_right_block() {
        let text = "kind: \"rcc\",\n  version: \"g4\",\nkind: \"flash\",\n  version: \"g4c3\",\n";
        assert_eq!(registers_version(text, "flash").as_deref(), Some("g4c3"));
        assert_eq!(registers_version(text, "pwr"), None);
    }

    /// Имена блоков — без повторов и в порядке появления: на этом порядке
    /// держится выбор между `IWDG1` и `IWDG2` у двухъядерных H7.
    #[test]
    fn watchdog_names_are_deduplicated_in_order() {
        // Один блок, названный дважды (у 154 наборов метаданных так и есть —
        // блок и одноимённое прерывание).
        assert_eq!(
            parse_watchdog_names("name: \"IWDG\",\n  name: \"IWDG\",\n"),
            vec!["IWDG".to_string()]
        );
        assert_eq!(
            parse_watchdog_names("name: \"IWDG1\",\n  name: \"IWDG2\",\n"),
            vec!["IWDG1".to_string(), "IWDG2".to_string()]
        );
        assert!(parse_watchdog_names("name: \"WWDG\",\n").is_empty());
    }

    /// Блок без выводов в корпусе — не интерфейс; FDCAN и bxCAN различаются
    /// по версии регистров, а не по имени блока.
    #[test]
    fn interfaces_come_from_blocks_with_pins() {
        let block = |kind: &str, version: &str, pins: &str| {
            format!(
                "    Peripheral {{
        name: \"X\",
        registers: Some(PeripheralRegisters {{
                             kind: \"{kind}\",
            version: \"{version}\",
        }}),
                         pins: &[{pins}],
    }},
"
            )
        };
        let pin = |signal: &str| {
            format!(
                "
            PeripheralPin {{ pin: \"PA11\", signal: \"{signal}\", af: Some(9) }},
        "
            )
        };
        let rx_tx = [pin("RX"), pin("TX")].concat();
        let dp_dm = [pin("DP"), pin("DM")].concat();
        let text = [
            block("otg", "v1", &dp_dm),
            block("can", "fdcan_h7", &rx_tx),
            block("eth", "v2", ""),
            block("usart", "v4", &rx_tx),
            block("usart", "v2", &rx_tx),
            block("can", "bxcan", &rx_tx),
            block("i2c", "v2", &rx_tx),
        ]
        .concat();
        assert_eq!(parse_interfaces(&text), ["usart", "fdcan", "bxcan", "otg"]);
    }

    /// Блоки с выводами нужных сигналов — в порядке предпочтения: `USART*`
    /// раньше `LPUART*`, `USB_OTG_FS` раньше `USB_OTG_HS`, блок без выводов не
    /// считается.
    #[test]
    fn instances_list_blocks_with_pins_in_preference_order() {
        let block = |name: &str, kind: &str, version: &str, pins: &str| {
            format!(
                "    Peripheral {{
        name: \"{name}\",
        registers: Some(PeripheralRegisters {{
                             kind: \"{kind}\",
            version: \"{version}\",
        }}),
                         pins: &[{pins}],
    }},
"
            )
        };
        let pin = |signal: &str| {
            format!(
                "
            PeripheralPin {{ pin: \"PA11\", signal: \"{signal}\", af: Some(9) }},
        "
            )
        };
        let rx_tx = [pin("RX"), pin("TX")].concat();
        let dp_dm = [pin("DP"), pin("DM")].concat();
        let text = [
            block("LPUART1", "usart", "v4", &rx_tx),
            block("USART3", "usart", "v4", &rx_tx),
            block("USART10", "usart", "v4", &rx_tx),
            block("USART2", "usart", "v4", &rx_tx),
            block("USB_OTG_HS", "otg", "v1", &dp_dm),
            block("USB_OTG_FS", "otg", "v1", ""),
            block("ETH", "eth", "v2", &pin("MDC")),
            block("FDCAN2", "can", "fdcan_h7", &rx_tx),
            block("FDCAN1", "can", "fdcan_h7", &rx_tx),
        ]
        .concat();
        let found = parse_instances(&text);
        assert_eq!(found["usart"], ["USART2", "USART3", "USART10", "LPUART1"]);
        assert_eq!(found["otg"], ["USB_OTG_HS"], "FS без выводов не годится");
        assert_eq!(found["eth"], ["ETH"]);
        assert_eq!(found["fdcan"], ["FDCAN1", "FDCAN2"]);
        assert!(!found.contains_key("usb"));
    }

    /// Блок годится, только если выведены оба сигнала интерфейса: `TX` и `RX`
    /// у USART и CAN, `DP` и `DM` у USB; у Ethernet — хоть один вывод.
    #[test]
    fn a_block_needs_the_pins_of_its_signals() {
        let block = |name: &str, kind: &str, version: &str, pins: &str| {
            format!(
                "    Peripheral {{
        name: \"{name}\",
        registers: Some(PeripheralRegisters {{
                             kind: \"{kind}\",
            version: \"{version}\",
        }}),
                         pins: &[{pins}],
    }},
"
            )
        };
        let pin = |signal: &str| {
            format!(
                "
            PeripheralPin {{ pin: \"PA11\", signal: \"{signal}\", af: Some(9) }},
        "
            )
        };
        let dp_dm = [pin("DP"), pin("DM")].concat();
        let text = [
            block("USART1", "usart", "v4", &pin("TX")),
            block("CAN1", "can", "bxcan", &pin("RX")),
            block("USB_OTG_FS", "otg", "v1", &dp_dm),
            block("ETH", "eth", "v2", &pin("MDC")),
        ]
        .concat();
        let found = parse_instances(&text);
        assert!(!found.contains_key("usart"));
        assert!(!found.contains_key("bxcan"));
        assert_eq!(found["otg"], ["USB_OTG_FS"]);
        assert_eq!(found["eth"], ["ETH"]);
        assert_eq!(parse_interfaces(&text), ["eth", "otg"]);
    }

    /// Прерывание RNG — у блока `rng`, а не у соседнего блока.
    #[test]
    fn rng_interrupt_comes_from_the_rng_block() {
        let text = "    Peripheral {
        registers: Some(PeripheralRegisters { kind: \"hash\", version: \"v1\" }),
        interrupts: &[PeripheralInterrupt { signal: \"GLOBAL\", interrupt: \"HASH_RNG\" }],
    },
    Peripheral {
        registers: Some(PeripheralRegisters { kind: \"rng\", version: \"v1\" }),
        interrupts: &[PeripheralInterrupt { signal: \"GLOBAL\", interrupt: \"AES_RNG\" }],
    },
";
        assert_eq!(parse_rng_interrupt(text).as_deref(), Some("AES_RNG"));
        assert_eq!(
            parse_rng_interrupt("    Peripheral {\n kind: \"usart\",\n"),
            None
        );
    }

    /// Линии с нулём мультиплексора «без клока» получают свой источник, а
    /// непроверенные — ничего.
    #[test]
    fn rng_clock_follows_the_line() {
        assert_eq!(rng_clock("f407ve"), Some(("pll48", "")));
        assert_eq!(rng_clock("g081rb"), Some(("rngsel", "SYS")));
        assert_eq!(rng_clock("wle5jc"), Some(("rngsel", "MSI")));
        assert_eq!(rng_clock("wba52cg"), Some(("rngsel", "HSI")));
        assert_eq!(rng_clock("wb55rg"), Some(("default", "")));
        assert_eq!(rng_clock("l476rg"), Some(("clk48sel", "MSI")));
        assert_eq!(rng_clock("l496zg"), Some(("default", "")));
        assert_eq!(rng_clock("l072cz"), Some(("clk48sel", "HSI48")));
        assert_eq!(rng_clock("h743zi"), Some(("default", "")));
        for unchecked in ["f410rb", "h7s3l8", "wb07cc", "u031r8", "c071rb"] {
            assert_eq!(rng_clock(unchecked), None, "{unchecked}");
        }
    }

    #[test]
    fn bank_mode_is_empty_only_for_single_config_chips() {
        let (regions, mode) = select_memory_config(configs(SINGLE_CONFIG)).expect("одна карта");
        assert_eq!(mode, "");
        assert_eq!(regions.len(), 2);

        // Из двух карт выбирается одобанковая — та, где есть BANK_1 и нет
        // BANK_2 (предикат повторяет build.rs embassy-stm32): непрерывный flash
        // и наибольший ACTIVE.
        let (regions, mode) = select_memory_config(configs(MULTI_CONFIG)).expect("две карты");
        assert_eq!(mode, "single-bank");
        assert_eq!(regions[0].size, 524288);
    }

    /// Флеш из двух банков по `bank` байт со страницей 2 KiB; второй — с
    /// адреса `bank2_origin` (вплотную к первому или за разрывом, как у G47x
    /// с 128/256 KiB).
    fn two_banks(bank: u64, bank2_origin: u64) -> Vec<RawRegion> {
        let mut regions = uniform_flash(bank, 2048, 8);
        regions.push(RawRegion {
            name: "BANK_2".to_string(),
            kind: RegionKind::Flash,
            address: bank2_origin,
            size: bank,
            flash: regions[0].flash,
        });
        regions
    }

    /// Банки стыкуются (H743ZI, L476): это один непрерывный flash, и ACTIVE с
    /// DFU делят его пополам, как у однобанкового, — DFU не привязан к банку 2.
    #[test]
    fn contiguous_banks_are_one_flash() {
        let bank = 256 * 1024;
        let layout =
            compute_memory_layout("test", &two_banks(bank, FLASH_BASE + bank)).expect("раскладка");
        assert_eq!(layout.flash_length, 2 * bank);
        assert_eq!(layout.dfu_bank_origin, None);
        let ota = layout.ota.expect("OTA помещается");
        assert_eq!(ota.dfu_origin, ota.active_origin + ota.active_length);
        assert!(ota.active_origin + ota.active_length > FLASH_BASE + bank);
    }

    /// Банк 2 за разрывом (как у H743 с 1 MiB: банк 2 с 0x0810_0000): цепочка
    /// для приложения — только банк 1, DFU — в банке 2, и хук узнаёт об этом
    /// по `dfu_bank_origin`.
    #[test]
    fn bank_2_behind_a_gap_still_takes_the_dfu() {
        let bank = 64 * 1024;
        let layout = compute_memory_layout("test", &two_banks(bank, FLASH_BASE + 0x4_0000))
            .expect("раскладка");
        assert_eq!(layout.flash_length, bank);
        assert_eq!(layout.dfu_bank_origin, Some(FLASH_BASE + 0x4_0000));
        let ota = layout.ota.expect("OTA помещается");
        assert_eq!(ota.dfu_origin, FLASH_BASE + 0x4_0000);
        assert!(ota.dfu_origin + ota.dfu_length <= FLASH_BASE + 0x4_0000 + bank);
        assert_eq!(ota.dfu_length, ota.active_length + 2048);

        // Настройки в хвосте — в конце банка 1, между ACTIVE и разрывом.
        let with_config = layout
            .config
            .expect("раздел")
            .ota
            .expect("OTA с настройками");
        let settings = with_config.settings.expect("раздел");
        assert!(settings.origin >= with_config.active_origin + with_config.active_length);
        assert_eq!(settings.origin + settings.length, FLASH_BASE + bank);
    }

    /// Один flash-регион, `size`/`erase_size` в байтах, RAM фиксирована в
    /// 64 KiB. Стирание в `0xFF` — как у подавляющего большинства семейств;
    /// обратный случай собирает [`flash_erasing_to_zero`].
    fn uniform_flash(size: u64, erase_size: u64, write_size: u64) -> Vec<RawRegion> {
        uniform_flash_with_ram(size, erase_size, write_size, 64 * 1024)
    }

    /// То же самое, но с произвольным размером RAM — нужен там, где важен
    /// именно он (например, границы `PANIC`), а не фиксированные 64 KiB.
    fn uniform_flash_with_ram(
        size: u64,
        erase_size: u64,
        write_size: u64,
        ram: u64,
    ) -> Vec<RawRegion> {
        let mut regions = flash_with_erase_value(size, erase_size, write_size, 0xFF);
        let sram = regions
            .iter_mut()
            .find(|r| r.kind == RegionKind::Ram)
            .expect("flash_with_erase_value всегда добавляет регион RAM");
        sram.size = ram;
        regions
    }

    /// Флеш, стирающийся в ноль (L0/L1, STM32WB10CC/WB15CC).
    fn flash_erasing_to_zero(size: u64, erase_size: u64, write_size: u64) -> Vec<RawRegion> {
        flash_with_erase_value(size, erase_size, write_size, 0x00)
    }

    fn flash_with_erase_value(
        size: u64,
        erase_size: u64,
        write_size: u64,
        erase_value: u8,
    ) -> Vec<RawRegion> {
        vec![
            RawRegion {
                name: "BANK_1".to_string(),
                kind: RegionKind::Flash,
                address: FLASH_BASE,
                size,
                flash: Some(FlashSettings {
                    erase_size,
                    write_size,
                    erase_value,
                }),
            },
            RawRegion {
                name: "SRAM".to_string(),
                kind: RegionKind::Ram,
                address: RAM_BASE,
                size: 64 * 1024,
                flash: None,
            },
        ]
    }

    /// Стёртое состояние берётся из метаданных, а не из имени чипа.
    ///
    /// Пока признак считался как «фича начинается на `l0`/`l1`», мимо него
    /// проходили STM32WB10CC/WB15CC: `erase_value: 0` при другом префиксе.
    /// Такому чипу доставался `embassy-boot` без `flash-erase-zero` (обмен
    /// разделов читает признак «стёрто» наоборот) и раздел настроек, который
    /// не работает вовсе, — обе поломки молчаливые.
    #[test]
    fn erase_zero_comes_from_the_metadata() {
        let ordinary = compute_memory_layout("test", &uniform_flash(512 * 1024, 2048, 8))
            .expect("раскладка для обычного флеша");
        assert!(!ordinary.erase_zero, "0xFF принят за ноль");

        let zeroing = compute_memory_layout("test", &flash_erasing_to_zero(512 * 1024, 2048, 8))
            .expect("раскладка для флеша, стирающегося в ноль");
        assert!(zeroing.erase_zero, "0x00 не распознан");
    }

    #[test]
    fn state_grows_with_active_to_fit_the_swap_journal() {
        // Страница обмена, не расширенная (физическая): журнал растёт с ACTIVE.
        // L476RG: 1 MiB при секторе 2 KiB, журналу нужно (2 + 4*247) слов по
        // 8 байт — одного сектора не хватает (раньше стоял ровно один, и
        // bootloader падал в панику на старте).
        let chain = uniform_flash(1024 * 1024, 2048, 8);
        let chain: Vec<&RawRegion> = chain
            .iter()
            .filter(|r| r.kind == RegionKind::Flash)
            .collect();
        let ota = compute_ota_at("test", &chain, 1024 * 1024, 2048, 8, false, None)
            .expect("OTA должна помещаться");
        assert!(ota.bootloader_state_length > 2048);
        assert_eq!(ota.bootloader_state_length, 8192);
    }

    /// Журнал не умещается в `STATE_BUDGET` — страница обмена вырастает, и
    /// `STATE` сжимается до сотен байт. 512 KiB при странице 2 KiB: 2 -> 4 KiB
    /// (на 2 KiB журналу нужно 3792 байта); L476RG с 1 MiB упирается в 8 KiB.
    #[test]
    fn swap_page_grows_until_the_journal_fits_the_budget() {
        let big = compute_memory_layout("test", &uniform_flash(1024 * 1024, 2048, 8))
            .expect("раскладка должна посчитаться");
        assert_eq!(big.ota.expect("OTA должна помещаться").swap_page_size, 8192);

        let layout = compute_memory_layout("test", &uniform_flash(512 * 1024, 2048, 8))
            .expect("раскладка должна посчитаться");
        assert_eq!(layout.page_size, 2048);
        let ota = layout.ota.expect("OTA должна помещаться");
        assert_eq!(ota.swap_page_size, 4096);
        assert!(journal_bytes(&ota, 8) <= STATE_BUDGET);
        assert!(ota.bootloader_state_length <= STATE_BUDGET);
        assert!(ota.bootloader_length >= BOOTLOADER_MIN);
        assert!(ota.active_origin.is_multiple_of(4096));
        assert!(ota.dfu_length >= ota.active_length + 4096 && ota.dfu_length.is_multiple_of(4096));
    }

    /// L073RZ: страница 128 байт, 192 KiB flash — без расширения `STATE` 10 KiB.
    #[test]
    fn tiny_pages_get_a_wide_swap_page_and_a_big_bootloader_zone() {
        let layout = compute_memory_layout("test", &flash_erasing_to_zero(192 * 1024, 128, 4))
            .expect("раскладка должна посчитаться");
        let ota = layout.ota.expect("OTA должна помещаться");
        assert!(ota.swap_page_size > 128 && ota.swap_page_size <= SWAP_PAGE_MAX);
        assert!(ota.bootloader_length >= BOOTLOADER_MIN);
        assert!(ota.bootloader_state_length <= STATE_BUDGET);
        assert_eq!(ota.bootloader_state_erase_size, 128);
    }

    /// Небольшой flash: журнал и так в бюджете, страница не трогается.
    #[test]
    fn swap_page_stays_physical_when_the_journal_already_fits() {
        let layout =
            compute_memory_layout("test", &uniform_flash(128 * 1024, 2048, 8)).expect("раскладка");
        let ota = layout.ota.expect("OTA помещается");
        assert_eq!(ota.swap_page_size, 2048);
        assert!(ota.bootloader_length >= BOOTLOADER_MIN);
    }

    /// F4 (неравные сектора) и банк 2 за разрывом страницу не расширяют.
    #[test]
    fn swap_page_is_not_widened_on_unequal_sectors_or_behind_a_bank_gap() {
        let f4 = compute_memory_layout("test", &f407ve()).expect("раскладка");
        assert_eq!(f4.ota.expect("OTA").swap_page_size, f4.page_size);
        assert_eq!(
            f4.config
                .expect("настройки")
                .ota
                .expect("OTA")
                .swap_page_size,
            f4.page_size
        );

        let gap = compute_memory_layout("test", &two_banks(64 * 1024, FLASH_BASE + 0x4_0000))
            .expect("раскладка");
        assert_eq!(gap.ota.expect("OTA").swap_page_size, gap.page_size);
    }

    /// Карта F407VE: 4×16 KiB, 64 KiB, дальше по 128 KiB. `STATE` — в
    /// последнем 16-килобайтном секторе (журналу хватает с запасом), перед ним
    /// bootloader, сектор 4 пустует. Со страницей всего чипа (128 KiB)
    /// `Partition::new` на таком разделе паниковал, поэтому раскладка обязана
    /// отдать страницу его собственного сектора.
    /// Карта F407VE: 4×16 KiB, 64 KiB, дальше 3×128 KiB.
    fn f407ve() -> Vec<RawRegion> {
        let sector = |name: &str, address: u64, size: u64, erase_size: u64| RawRegion {
            name: name.to_string(),
            kind: RegionKind::Flash,
            address,
            size,
            flash: Some(FlashSettings {
                erase_size,
                write_size: 4,
                erase_value: 0xFF,
            }),
        };
        let mut regions = uniform_flash(0, 1, 4);
        regions.retain(|r| r.kind == RegionKind::Ram);
        regions.extend([
            sector("BANK_1_REGION_1", FLASH_BASE, 64 * 1024, 16 * 1024),
            sector(
                "BANK_1_REGION_2",
                FLASH_BASE + 0x1_0000,
                64 * 1024,
                64 * 1024,
            ),
            sector(
                "BANK_1_REGION_3",
                FLASH_BASE + 0x2_0000,
                384 * 1024,
                128 * 1024,
            ),
        ]);
        regions
    }

    #[test]
    fn state_takes_the_last_smallest_sector_with_its_own_page() {
        let layout =
            compute_memory_layout("test", &f407ve()).expect("раскладка должна посчитаться");
        assert_eq!(layout.page_size, 128 * 1024);
        let ota = layout.ota.expect("OTA должна помещаться");
        assert_eq!(ota.bootloader_length, 48 * 1024);
        assert_eq!(ota.bootloader_state_origin, FLASH_BASE + 0xC000);
        assert_eq!(ota.bootloader_state_length, 16 * 1024);
        assert_eq!(ota.bootloader_state_erase_size, 16 * 1024);
        assert_eq!(ota.active_origin, FLASH_BASE + 0x2_0000);

        // С разделом настроек — вариант A: bootloader 0–1, настройки 2–3
        // (2×16 KiB), STATE в секторе 4. ACTIVE тот же, что без настроек, а в
        // хвосте раздел стоил бы 2×128 KiB и вытеснил бы OTA вовсе.
        let config = layout.config.expect("раздел настроек возможен");
        let ota = config.ota.expect("OTA с настройками должна помещаться");
        assert_eq!(ota.bootloader_length, 32 * 1024);
        assert_eq!(
            ota.settings,
            Some(SettingsPartition {
                origin: FLASH_BASE + 0x8000,
                length: 32 * 1024,
                erase_size: 16 * 1024,
            })
        );
        assert_eq!(ota.bootloader_state_origin, FLASH_BASE + 0x1_0000);
        assert_eq!(ota.bootloader_state_erase_size, 64 * 1024);
        assert_eq!(ota.active_origin, FLASH_BASE + 0x2_0000);
        assert_eq!(ota.active_length, 128 * 1024);
        // Без OTA раздел по-прежнему в хвосте.
        assert_eq!(config.tail.origin, FLASH_BASE + 0x4_0000);
        assert_eq!(config.tail.erase_size, 128 * 1024);
    }

    /// Пятая проверка — от `Partition::new`: раздел, не кратный странице флеша
    /// под ним, ловится при генерации, а не паникой на плате.
    #[test]
    #[should_panic(expected = "Partition::new")]
    fn state_misaligned_to_its_page_is_caught() {
        let state_on_the_chip_page = OtaPartitions {
            bootloader_length: 64 * 1024,
            bootloader_state_origin: FLASH_BASE + 0x1_0000,
            bootloader_state_length: 64 * 1024,
            bootloader_state_erase_size: 128 * 1024,
            swap_page_size: 128 * 1024,
            active_origin: FLASH_BASE + 0x2_0000,
            active_length: 128 * 1024,
            dfu_origin: FLASH_BASE + 0x4_0000,
            dfu_length: 256 * 1024,
            settings: None,
        };
        assert_embassy_boot_invariants("test", &state_on_the_chip_page, 128 * 1024, 4);
    }

    /// Мелкие сектора у самого начала (4×4 KiB): bootloader'у перед последним
    /// из них досталось бы 12 KiB, меньше `BOOTLOADER_MIN`. Тогда `STATE` —
    /// в следующем по размеру секторе, а не отказ от OTA.
    #[test]
    fn state_falls_back_to_a_larger_sector_when_small_ones_are_too_early() {
        let sector = |offset: u64| -> Option<u64> {
            Some(if offset < 16 * 1024 {
                4 * 1024
            } else {
                64 * 1024
            })
        };
        let sectors = reserve_sectors(&sector, 144 * 1024).expect("сектора известны");
        assert_eq!(
            place_state(&sectors, 64, BOOTLOADER_MIN),
            Some((80 * 1024, 64 * 1024, 64 * 1024))
        );
    }

    /// Внешняя раскладка на F407VE (4×16, 64, 3×128 KiB): bootloader — до
    /// первой 128-килобайтной границы, `ACTIVE` — всё остальное, снаружи
    /// `STATE`, `DFU`, настройки по 128 KiB (страница обмена крупнее блока).
    #[test]
    fn external_layout_keeps_everything_but_the_bootloader_for_active() {
        let e =
            compute_external_ota("test", &[], 512 * 1024, 128 * 1024, false).expect("помещается");
        assert_eq!(e.bootloader_length, 128 * 1024);
        assert_eq!(e.active_origin, FLASH_BASE + 0x2_0000);
        assert_eq!(e.active_length, 384 * 1024);
        assert_eq!(e.page_size, 128 * 1024);
        assert_eq!(e.ext_align, 128 * 1024);
        assert_eq!((e.state_offset, e.state_length), (0, 128 * 1024));
        assert_eq!((e.dfu_offset, e.dfu_length), (128 * 1024, 512 * 1024));
        assert_eq!(e.config_offset, 640 * 1024);
    }

    /// Мелкие страницы (G0: 2 KiB): bootloader ровно `BOOTLOADER_TARGET`,
    /// `ACTIVE` кратен 4 KiB — стиранию внешней NOR, — а не 2 KiB чипа.
    #[test]
    fn external_layout_on_small_pages_rounds_active_to_the_external_sector() {
        let e =
            compute_external_ota("test", &[], 128 * 1024 + 2048, 2048, false).expect("помещается");
        assert_eq!(e.bootloader_length, 32 * 1024);
        assert_eq!(e.active_length, 96 * 1024);
        assert_eq!(e.page_size, 4 * 1024);
        assert_eq!(e.ext_align, 64 * 1024);
        assert_eq!(e.dfu_length, 128 * 1024);
        // Журнал — на страницу чипа: (2 + 4*48) слов по 32 байта — 6208
        // байт, один блок.
        assert_eq!(e.state_length, 64 * 1024);
    }

    #[test]
    fn external_layout_reports_a_chip_too_small_for_it() {
        let note =
            compute_external_ota("test", &[], 32 * 1024, 1024, false).expect_err("не помещается");
        assert!(note.contains("ни страницы"), "{note}");
    }

    /// Настройки во внутреннем флеше при внешней OTA на F407VE: сектора 2–3
    /// (2×16 KiB) зоны bootloader'а, которые иначе пропадают; `ACTIVE` тот же,
    /// что без настроек.
    #[test]
    fn external_layout_puts_internal_settings_into_the_bootloader_zone() {
        let layout = compute_memory_layout("test", &f407ve()).expect("раскладка");
        let e = layout.external_config.expect("помещается");
        assert_eq!(
            e.settings,
            Some(SettingsPartition {
                origin: FLASH_BASE + 0x8000,
                length: 32 * 1024,
                erase_size: 16 * 1024,
            })
        );
        assert_eq!(e.bootloader_length, 32 * 1024);
        assert_eq!(e.active_origin, FLASH_BASE + 0x2_0000);
        assert_eq!(e.active_length, 384 * 1024);
    }

    /// Равные мелкие страницы (G0: 2 KiB): в зоне bootloader'а за 32 KiB
    /// места нет, настройки — две последние страницы, `ACTIVE` короче.
    #[test]
    fn external_layout_puts_internal_settings_at_the_tail_on_uniform_pages() {
        let layout =
            compute_memory_layout("test", &uniform_flash(128 * 1024, 2048, 8)).expect("раскладка");
        let e = layout.external_config.expect("помещается");
        assert_eq!(
            e.settings,
            Some(SettingsPartition {
                origin: FLASH_BASE + 124 * 1024,
                length: 4 * 1024,
                erase_size: 2048,
            })
        );
        assert_eq!(e.bootloader_length, 32 * 1024);
        assert_eq!(e.active_length, 92 * 1024);
    }

    #[test]
    fn rust_literal_groups_digits_by_three() {
        assert_eq!(rust_literal(128), "128");
        assert_eq!(rust_literal(16_384), "16_384");
        assert_eq!(rust_literal(131_072), "131_072");
        assert_eq!(rust_literal(1_048_576), "1_048_576");
    }

    #[test]
    fn chip_with_too_few_sectors_reports_why() {
        // H723VE: 512 KiB одним регионом с сектором 128 KiB — 4 сектора, а
        // схеме нужно минимум 5 (BOOTLOADER + STATE + ACTIVE + 2×DFU).
        let layout = compute_memory_layout("test", &uniform_flash(512 * 1024, 128 * 1024, 32))
            .expect("раскладка должна посчитаться");
        let note = layout.ota.expect_err("OTA не должна помещаться");
        assert!(note.contains("512 KiB"), "{note}");
        assert!(note.contains("нужно 3"), "{note}");
        // Но FLASH/RAM всё равно посчитаны — memory.x не остаётся заглушкой.
        assert_eq!(layout.flash_length, 512 * 1024);
        assert_ne!(layout.panic.1, 0, "дамп паники — единственный резерв в RAM");
        assert_eq!(
            layout.panic.0,
            layout.ram_origin + layout.ram_length,
            "PANIC начинается там, где кончается RAM"
        );
    }

    /// Резерв — доля RAM, зажатая между минимумом и максимумом: на 2 KiB это
    /// 128 байт (не половина памяти), на 8 KiB и больше — 512 (сообщение всё
    /// равно обрезается по региону).
    #[test]
    fn panic_region_is_a_clamped_fraction_of_ram() {
        for (ram, expected) in [
            (2 * 1024, 128),
            (4 * 1024, 256),
            (8 * 1024, 512),
            (192 * 1024, 512),
        ] {
            let layout =
                compute_memory_layout("test", &uniform_flash_with_ram(512 * 1024, 2048, 8, ram))
                    .expect("раскладка должна посчитаться");
            assert_eq!(layout.panic.1, expected, "RAM {ram}");
            assert_eq!(layout.ram_length, ram - expected, "RAM {ram}");
        }
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

    /// Форма с управлением пробелами — та, что стоит в `crates-cross/bsp/Cargo.toml`.
    /// Пока её не понимали, `cargo metadata` падал на невалидном TOML, а с ним
    /// и весь генератор данных; заметно это только при его запуске, то есть
    /// раз в обновление embassy-stm32.
    #[test]
    fn liquid_whitespace_control_is_stripped_too() {
        let manifest = "[dependencies]\n{%- if ota == \"true\" %}\nembassy-boot = \"1\"\n{%- endif %}\ndefmt = \"1\"\n";
        let stripped = strip_liquid(manifest);
        assert!(!stripped.contains("embassy-boot"), "{stripped}");
        assert!(!stripped.contains('%'), "тег остался в тексте: {stripped}");
        assert!(stripped.contains("defmt"), "{stripped}");
    }

    /// Вложенный `{% if %}` — ровно та форма, что стоит в
    /// `crates-cross/bsp/Cargo.toml`: условная фича `flash-erase-zero` внутри
    /// списка `features`, а весь блок — внутри условия по `ota`/`signed`.
    ///
    /// Пока `strip_liquid` искал ближайший `{% endif %}`, а не парный, он
    /// резал от внешнего открывающего до внутреннего закрывающего: в тексте
    /// оставался лишний `{% endif %}`, то есть невалидный TOML, и генератор
    /// данных умирал на `cargo metadata`. Заметно это было только при его
    /// запуске — раз в обновление embassy-stm32.
    #[test]
    fn nested_liquid_conditionals_are_stripped_as_a_whole() {
        let manifest = concat!(
            "[dependencies]\n",
            "{%- if ota == \"true\" %}\n",
            "embassy-boot = { features = [\"defmt\"",
            "{% if erase_zero == \"true\" %}, \"flash-erase-zero\"{% endif %}] }\n",
            "{%- endif %}\n",
            "defmt = \"1\"\n",
        );

        let stripped = strip_liquid(manifest);

        assert!(!stripped.contains('%'), "тег остался в тексте: {stripped}");
        assert!(
            !stripped.contains("embassy-boot"),
            "внешний блок должен уйти целиком: {stripped}"
        );
        assert!(
            stripped.contains("defmt = \"1\""),
            "срезано лишнее: {stripped}"
        );
    }

    /// Вложенность снимается и когда внешний блок остаётся единственным на
    /// весь файл, и когда за ним идёт ещё один такой же: счёт глубины не
    /// должен «залипать» между блоками.
    #[test]
    fn two_nested_blocks_in_a_row_are_stripped() {
        let manifest = concat!(
            "a = 1\n",
            "{% if x %}{% if y %}inner{% endif %}outer{% endif %}\n",
            "{% if x %}{% if y %}inner{% endif %}outer{% endif %}\n",
            "b = 2\n",
        );

        assert_eq!(strip_liquid(manifest), "a = 1\n\n\nb = 2\n");
    }

    /// Незакрытый тег оставляется как есть — падать должен `cargo metadata`
    /// с понятной ошибкой, а не разбор здесь с невнятной.
    #[test]
    fn an_unclosed_tag_is_left_alone() {
        let manifest = "a = 1\n{% if x %}\nb = 2\n";

        assert_eq!(strip_liquid(manifest), manifest);
    }

    /// Все Liquid-теги в манифестах шаблона снимаются: если появится форма,
    /// которую `strip_liquid` не понимает, тест увидит это здесь, а не через
    /// полгода при обновлении зависимостей.
    #[test]
    fn every_template_manifest_strips_clean() {
        let root = repo_root();
        let mut checked = 0;
        for manifest in [
            root.join("crates-cross/Cargo.toml"),
            root.join("crates-cross/bsp/Cargo.toml"),
            root.join("crates-cross/app/Cargo.toml"),
            root.join("crates-cross/boot/Cargo.toml"),
            root.join("crates-cross/target-tests/Cargo.toml"),
            root.join("crates-host/chip-info/Cargo.toml"),
        ] {
            let Ok(text) = fs::read_to_string(&manifest) else {
                continue;
            };
            checked += 1;
            let stripped = strip_liquid(&text);
            assert!(
                !stripped.contains("{%"),
                "в {} остался Liquid-тег после strip_liquid:\n{stripped}",
                manifest.display(),
            );
        }
        assert!(checked >= 5, "манифесты шаблона не нашлись: {checked}");
    }

    /// Каталог заменяется целиком: новые файлы на месте, файл, которого в
    /// новом наборе нет, исчез, временных каталогов не осталось.
    #[test]
    fn data_dir_is_replaced_as_a_whole() {
        let root = std::env::temp_dir().join(format!("chip-data-gen-write-{}", std::process::id()));
        drop(fs::remove_dir_all(&root));
        let dir = root.join("chip-data");
        let file = |relative: &str, body: &str| (relative.to_owned(), body.to_owned());

        write_data_dir(
            &dir,
            &[file("chips.rhai", "old"), file("layout/f1.rhai", "f1")],
        )
        .expect("первая запись");
        write_data_dir(
            &dir,
            &[file("chips.rhai", "new"), file("layout/f4.rhai", "f4")],
        )
        .expect("вторая запись");

        assert_eq!(
            fs::read_to_string(dir.join("chips.rhai")).expect("chips"),
            "new"
        );
        assert_eq!(
            fs::read_to_string(dir.join("layout/f4.rhai")).expect("f4"),
            "f4"
        );
        assert!(
            !dir.join("layout/f1.rhai").exists(),
            "устаревший файл остался"
        );
        assert!(
            !root.join("chip-data.tmp").exists(),
            "временный каталог остался"
        );
        assert!(
            !root.join("chip-data.old").exists(),
            "старый каталог остался"
        );

        drop(fs::remove_dir_all(&root));
    }

    #[test]
    fn chip_features_are_told_from_service_features() {
        for yes in [
            "stm32f407ve",
            "stm32h745zi-cm7",
            "stm32l151c6-a",
            "stm32c011d6",
        ] {
            assert!(is_chip_feature(yes), "{yes}");
        }
        for no in [
            "stm32",
            "stm32-cm7",
            "stm32f4-",
            "stm32a-b-c",
            "stm32F407",
            "f407ve",
            "defmt",
            "stm32f4_x",
        ] {
            assert!(!is_chip_feature(no), "{no}");
        }
    }

    #[test]
    fn package_candidates_are_longer_targets_ending_in_the_grade_letter() {
        let chips: BTreeSet<String> = [
            "STM32L151C6",
            "STM32L151C6TxA",
            "STM32L151C6UxA",
            "STM32L151C6TxB",
            "STM32L151C8TxA",
            "STM32H745ZI",
        ]
        .into_iter()
        .map(String::from)
        .collect();
        assert_eq!(
            package_candidates("l151c6-a", &chips),
            vec!["STM32L151C6TxA".to_string(), "STM32L151C6UxA".to_string()]
        );
        // Ядерный маркер и суффикс без дефиса сюда не доходят.
        assert!(package_candidates("h745zi-cm4", &chips).is_empty());
        assert!(package_candidates("l151c6", &chips).is_empty());
        // Градация, для которой нет более точной цели.
        assert!(package_candidates("l151c6-c", &chips).is_empty());
    }

    /// Раздел настроек — две последние страницы; OTA считается от того, что
    /// осталось. Не кратный странице flash или отсутствие места под приложение —
    /// `None`.
    #[test]
    fn config_partition_takes_the_last_two_pages() {
        let regions = uniform_flash(512 * 1024, 2048, 8);
        let chain: Vec<&RawRegion> = regions
            .iter()
            .filter(|r| r.kind == RegionKind::Flash)
            .collect();
        let config = compute_config_partition("test", &chain, 512 * 1024, 2048, 8, None)
            .expect("раздел должен поместиться");
        assert_eq!(config.tail.length, 4096);
        assert_eq!(config.flash_length, 512 * 1024 - 4096);
        assert_eq!(config.tail.origin, FLASH_BASE + config.flash_length);

        // Не кратно странице.
        assert!(compute_config_partition("test", &chain, 512 * 1024 + 8, 2048, 8, None).is_none());
        // Две страницы целиком уходят под раздел — приложению ничего.
        assert!(compute_config_partition("test", &chain, 2 * 2048, 2048, 8, None).is_none());
        // Ровно одна страница остаётся приложению — граница включительно.
        assert!(compute_config_partition("test", &chain, 3 * 2048, 2048, 8, None).is_some());
    }

    fn region(name: &str, kind: RegionKind, address: u64, size: u64) -> RawRegion {
        RawRegion {
            name: name.to_string(),
            kind,
            address,
            size,
            flash: None,
        }
    }

    #[test]
    fn extra_regions_skip_covered_and_comment_out_aliases_and_external_buses() {
        let regions = [
            region("SRAM", RegionKind::Ram, RAM_BASE, 64 * 1024),
            region("CCMRAM", RegionKind::Ram, 0x1000_0000, 64 * 1024),
            region("SRAM2", RegionKind::Ram, 0x3000_0000, 16 * 1024),
            region("SRAM2_ICODE", RegionKind::Ram, 0x1000_8000, 16 * 1024),
            region("FMC_BANK1", RegionKind::Ram, 0x6000_0000, 262_144_000),
            region("EMPTY", RegionKind::Ram, 0x4000_0000, 0),
            region("EEPROM", RegionKind::Eeprom, 0x0808_0000, 2048),
        ];
        // SRAM (основная RAM) покрыта хуком, EMPTY нулевого размера пропущен.
        let lines = extra_region_lines(
            &regions,
            FLASH_BASE + 512 * 1024,
            RAM_BASE + 64 * 1024,
            None,
        );
        let joined = lines.join("\n");
        assert_eq!(lines.len(), 5, "{joined}");
        assert!(!joined.contains("EMPTY"), "{joined}");
        assert!(!joined.contains(" SRAM "), "{joined}");
        let find = |name: &str| {
            lines
                .iter()
                .find(|l| l.contains(name))
                .unwrap_or_else(|| panic!("{name} нет в:\n{joined}"))
        };
        // Обычный регион — рабочая строка с правами по виду.
        assert!(find("CCMRAM").contains("(xrw) : ORIGIN = 0x10000000, LENGTH = 64K"));
        assert!(!find("CCMRAM").contains("/*"));
        assert!(find("EEPROM").contains("(r)"));
        // Алиас и внешняя шина закомментированы.
        assert!(find("SRAM2_ICODE").contains("/* "));
        assert!(find("SRAM2_ICODE").contains("второе окно"));
        assert!(find("FMC_BANK1").contains("внешняя шина"));
        // Сортировка по адресу: CCMRAM (0x1000_0000) раньше SRAM2_ICODE (0x1000_8000).
        assert!(joined.find("CCMRAM") < joined.find("SRAM2_ICODE"));
    }

    /// Нарушенный инвариант называет чип — иначе по одной панике на 1479 чипов
    /// не понять, чья раскладка сломана.
    #[test]
    #[should_panic(expected = "STM32TEST")]
    fn boot_invariant_panic_names_the_chip() {
        let broken = OtaPartitions {
            bootloader_length: 0,
            bootloader_state_origin: FLASH_BASE,
            bootloader_state_length: 2048,
            bootloader_state_erase_size: 2048,
            swap_page_size: 2048,
            active_origin: FLASH_BASE + 2048,
            active_length: 3000,
            dfu_origin: FLASH_BASE + 2048 + 3000,
            dfu_length: 6000,
            settings: None,
        };
        assert_embassy_boot_invariants("STM32TEST", &broken, 2048, 8);
    }

    /// `CORE_MARKERS` (какие суффиксы считать ядром, а не градацией) и
    /// `core_override()` в `chip-select.rhai` (какие ядра он знает) — один и тот
    /// же список в двух местах.
    #[test]
    fn core_markers_match_core_override_in_the_hook() {
        let script = fs::read_to_string(repo_root().join("chip-select.rhai"))
            .expect("chip-select.rhai читается");
        let start = script
            .find("fn core_override(marker) {")
            .expect("core_override не найден");
        let body = &script[start..];
        let body = &body[..body.find("\n}").expect("конец core_override")];
        let mut in_hook: Vec<&str> = body
            .lines()
            .filter_map(|line| {
                let (key, _) = line.trim().split_once(" => ")?;
                key.strip_prefix('"')?.strip_suffix('"')
            })
            .collect();
        in_hook.sort_unstable();
        let mut in_generator = CORE_MARKERS.to_vec();
        in_generator.sort_unstable();
        assert_eq!(in_hook, in_generator);
    }
}
