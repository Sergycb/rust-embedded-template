//! Один прогон генератора: собрать факты по каждому чипу и записать
//! `chip-data/`.

use std::{collections::BTreeMap, fs, path::PathBuf};

use anyhow::Context;

use crate::{
    emit::{
        DATA_DIR, DEFAULT_WATCHDOG, format_bank_modes, format_bank_switch, format_caches,
        format_chip_list, format_cpu_freq_boost, format_instances, format_interfaces,
        format_memory_layouts, format_package_choices, format_resets, format_rngs,
        format_source_stamp, format_watchdogs, write_data_dir,
    },
    layout::{MemoryLayout, compute_memory_layout},
    metadata::{
        RngClock, SharedMetadata, chip_caches, chip_watchdog, parse_chip_memory, reset_register,
        rng_clock, select_memory_config, shared_metadata,
    },
    source::{
        declared_embassy_version, package_candidates, probe_rs_candidate, probe_rs_chip_names,
        probe_rs_version, repo_root, resolve_cargo_metadata,
    },
};

/// Семейства, у которых двухбанковый режим стоит с завода, — по версии блока
/// FLASH в метаданных, с именем варианта регистров option bytes в
/// `crates-cross/bsp/src/banks.rs`. Заводское значение — знание документации, а
/// не метаданных: `DBANK`/`DUAL_BANK` = 1 в FLASH_OPTR нового кристалла (RM0440
/// G4, RM0444 G0, RM0432 L4+, RM0438 L5; OpenOCD `stm32l4x.c`). У F4 с 1 MiB
/// (`DB1M` = 0) и F76x/F77x (`nDBANK` = 1) с завода одобанковый, и их здесь нет.
const FACTORY_DUAL_BANK: &[(&str, &str)] =
    &[("g0x1", "g0"), ("g4c3", "g4"), ("l4", "l4"), ("l5", "l5")];

/// Перегенерирует `chip-data/` целиком: см. MAINTAINING.md, «Когда перегенерировать данные».
///
/// # Errors
///
/// Не разобрались метаданные или источники (`cargo metadata`, `probe-rs`),
/// не посчиталась раскладка хоть одного чипа, не записался каталог.
pub fn run() -> anyhow::Result<()> {
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
    let mut resets: BTreeMap<&str, String> = BTreeMap::new();
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
        // Причину сброса `bsp` читает у каждого чипа: нет блока `RCC` —
        // сломанное предположение о метаданных, а не «чип без флагов».
        let rcc = shared
            .rcc_version
            .clone()
            .with_context(|| format!("чип {suffix}: в {} нет блока rcc", shared_path.display()))?;
        resets.insert(suffix, rcc);
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
    let mut reset_regs: BTreeMap<&str, (String, String)> = BTreeMap::new();
    for version in resets.values() {
        if !reset_regs.contains_key(version.as_str()) {
            let path = cargo_metadata
                .stm32_metapac_chips_dir
                .parent()
                .context("у каталога chips нет родителя")?
                .join("peripherals")
                .join(format!("rcc_{version}.rs"));
            let text = fs::read_to_string(&path)
                .with_context(|| format!("не удалось прочитать {}", path.display()))?;
            let register = reset_register(&text).with_context(|| format!("{}", path.display()))?;
            reset_regs.insert(version, register);
        }
    }
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
        ("reset.rhai".to_owned(), format_resets(&resets, &reset_regs)),
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
