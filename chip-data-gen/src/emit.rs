//! Тексты таблиц `chip-data/*.rhai` и запись каталога целиком.

use std::{
    collections::BTreeMap,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
};

use anyhow::Context;

use crate::{
    layout::{ExternalOta, MemoryLayout, OtaPartitions, format_addr, format_size},
    metadata::{INTERFACE_TAGS, RngClock},
};

/// Строка-штамп с версиями источников данных. Её сверяет тест
/// `generated_blocks_match_the_declared_embassy_version`: без штампа связь
/// «подняли embassy-stm32 — перегенерируйте списки» держалась только на памяти
/// мейнтейнера, а поднимает версию обычно бот, молча.
pub const SOURCE_STAMP_PREFIX: &str = "// Источник: embassy-stm32 ";

/// Каталог с таблицами, которые `chip-select.rhai` подключает через `import`.
/// Пишется целиком при каждом прогоне — руками там ничего не правится.
pub(crate) const DATA_DIR: &str = "chip-data";

/// Имя блока независимого сторожевого таймера у подавляющего большинства
/// чипов (800 из 891 наборов метаданных). Таблица `WATCHDOG` перечисляет
/// только исключения, а хук подставляет это значение для всех остальных — по
/// той же схеме, что и `BANK_MODE`.
pub(crate) const DEFAULT_WATCHDOG: &str = "IWDG";

/// Первая строка каждого файла в [`DATA_DIR`].
const DATA_HEADER: &str = "// Сгенерировано chip-data-gen, руками не править: cargo run --manifest-path chip-data-gen/Cargo.toml\n";

/// Раскладки по файлам семейств: ключ — [`layout_family`], значение — текст
/// `chip-data/layout/<ключ>.rhai`. Одной таблицей это ~75 тысяч строк, а хуку
/// нужна одна запись — он импортирует только файл своего семейства.
pub(crate) fn format_memory_layouts(
    layouts: &BTreeMap<&str, MemoryLayout>,
) -> BTreeMap<String, String> {
    let mut files: BTreeMap<String, String> = BTreeMap::new();
    for (suffix, m) in layouts {
        let mut out = String::new();
        writeln!(out, "    \"{suffix}\": #{{").expect("запись в String не отказывает");
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
                writeln!(out, "            \"{line}\",").expect("запись в String не отказывает");
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
        // Только у чипов с регионом `DMA_NC` (`SRAM4` под BDMA — у H7): по
        // наличию ключа хук включает `dma_nc` и `bdma_nc`.
        if let Some(dma) = &m.dma_nc {
            push_field(&mut out, "dma_nc_origin", &format_addr(dma.origin));
            push_field(&mut out, "dma_nc_length", &format_size(dma.length));
        }
        if let Some((origin, length)) = m.bdma_nc {
            push_field(&mut out, "bdma_nc_origin", &format_addr(origin));
            push_field(&mut out, "bdma_nc_length", &format_size(length));
        }
        // Только у чипов с разрывом между банками — по наличию ключа хук и
        // решает, что DFU пишется через регион банка.
        if let Some(origin) = m.dfu_bank_origin {
            push_field(&mut out, "dfu_bank_origin", &format_addr(origin));
        }
        match &m.ota {
            Ok(p) => push_ota(&mut out, "", p, m.page_size),
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
                        push_ota(&mut out, "config_", p, m.page_size);
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
        // Третий набор — OTA с `DFU`/`STATE` на внешней флеши (`ExternalOta`,
        // `ext_`): настройки тогда снаружи или их нет.
        push_external(&mut out, "ext_", &m.external);
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

/// Только чипы, у которых блок сторожевого таймера зовётся НЕ `IWDG` — для
/// остальных хук подставляет [`DEFAULT_WATCHDOG`]. Полная таблица на все
/// чипы повторяла бы одно и то же слово полутора тысячами строк.
pub(crate) fn format_watchdogs(watchdogs: &BTreeMap<&str, String>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    writeln!(
        out,
        "// {} шт., остальным — {DEFAULT_WATCHDOG}",
        watchdogs.len()
    )
    .expect("запись в String не отказывает");
    out.push_str("export const WATCHDOG = #{\n");
    for (suffix, name) in watchdogs {
        writeln!(out, "    \"{suffix}\": \"{name}\",").expect("запись в String не отказывает");
    }
    out.push_str("};\n");
    out
}

/// RNG каждого чипа, у которого он подключается (см. [`rng_clock`](crate::metadata::rng_clock)):
/// прерывание, способ дать клок и вариант мультиплексора.
pub(crate) fn format_rngs(chips: &BTreeMap<&str, (String, RngClock)>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    writeln!(out, "// {} шт.: [прерывание, клок, источник]", chips.len())
        .expect("запись в String не отказывает");
    out.push_str("export const RNG = #{\n");
    for (suffix, (interrupt, (clock, source))) in chips {
        writeln!(
            out,
            "    \"{suffix}\": [\"{interrupt}\", \"{clock}\", \"{source}\"],"
        )
        .expect("запись в String не отказывает");
    }
    out.push_str("};\n");
    out
}

/// Интерфейсы связи каждого чипа (см. `metadata::parse_interfaces`): по ним хук
/// проверяет ответ на вопрос об интерфейсе, а `bsp` выбирает драйвер.
pub(crate) fn format_interfaces(chips: &BTreeMap<&str, Vec<&'static str>>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    writeln!(
        out,
        "// {} шт.; метки: {}",
        chips.len(),
        INTERFACE_TAGS.join(", ")
    )
    .expect("запись в String не отказывает");
    out.push_str("export const INTERFACES = #{\n");
    for (suffix, tags) in chips {
        let tags: Vec<String> = tags.iter().map(|t| format!("\"{t}\"")).collect();
        writeln!(out, "    \"{suffix}\": [{}],", tags.join(", "))
            .expect("запись в String не отказывает");
    }
    out.push_str("};\n");
    out
}

/// Имена блоков интерфейсов каждого чипа (см. `metadata::parse_instances`): по ним
/// `resources.rs` кладёт блок выбранного интерфейса в свою группу.
pub(crate) fn format_instances(
    chips: &BTreeMap<&str, BTreeMap<&'static str, Vec<String>>>,
) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    writeln!(
        out,
        "// {} шт.; метка интерфейса: имена блоков в порядке выбора",
        chips.len()
    )
    .expect("запись в String не отказывает");
    out.push_str("export const INSTANCES = #{\n");
    for (suffix, names) in chips {
        let names: Vec<String> = names
            .iter()
            .map(|(tag, blocks)| {
                let blocks: Vec<String> = blocks.iter().map(|b| format!("\"{b}\"")).collect();
                format!("\"{tag}\": [{}]", blocks.join(", "))
            })
            .collect();
        writeln!(out, "    \"{suffix}\": #{{{}}},", names.join(", "))
            .expect("запись в String не отказывает");
    }
    out.push_str("};\n");
    out
}

/// Чипы с option byte `CPU_FREQ_BOOST` (см. [`SharedMetadata::cpu_freq_boost`](crate::metadata::SharedMetadata::cpu_freq_boost))
/// — массивом, а не таблицей: значение у всех одно, важна только
/// принадлежность.
pub(crate) fn format_cpu_freq_boost(chips: &[&str]) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    writeln!(out, "// {} шт.", chips.len()).expect("запись в String не отказывает");
    out.push_str("export const CPU_FREQ_BOOST = [\n");
    for suffix in chips {
        writeln!(out, "    \"{suffix}\",").expect("запись в String не отказывает");
    }
    out.push_str("];\n");
    out
}

/// Кэши чипов (см. [`chip_caches`](crate::metadata::chip_caches)) — теги через пробел, как их читает Liquid
/// (`cache contains "icen"`); чипам без кэша записи нет.
pub(crate) fn format_caches(chips: &BTreeMap<&str, Vec<&'static str>>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    writeln!(out, "// {} шт.", chips.len()).expect("запись в String не отказывает");
    out.push_str("export const CACHE = #{\n");
    for (suffix, tokens) in chips {
        writeln!(out, "    \"{suffix}\": \"{}\",", tokens.join(" "))
            .expect("запись в String не отказывает");
    }
    out.push_str("};\n");
    out
}

/// Флаги сброса (см. [`reset_register`](crate::metadata::reset_register)): у чипа — версия `RCC`, у версии —
/// аксессор регистра и пары флагов. Две таблицы, а не одна: версий четыре
/// десятка, чипов — полторы тысячи.
pub(crate) fn format_resets(
    chips: &BTreeMap<&str, String>,
    registers: &BTreeMap<&str, (String, String)>,
) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    writeln!(out, "// {} шт.", chips.len()).expect("запись в String не отказывает");
    out.push_str("export const RESET = #{\n");
    for (suffix, version) in chips {
        writeln!(out, "    \"{suffix}\": \"{version}\",").expect("запись в String не отказывает");
    }
    out.push_str("};\nexport const RESET_REGS = #{\n");
    for (version, (register, flags)) in registers {
        writeln!(out, "    \"{version}\": [\"{register}\", \"{flags}\"],")
            .expect("запись в String не отказывает");
    }
    out.push_str("};\n");
    out
}

/// Чипы, которым прошивка переключает флеш в однобанковый режим, — с вариантом
/// регистров (см. `generate::FACTORY_DUAL_BANK`) и битом режима через двоеточие.
pub(crate) fn format_bank_switch(chips: &BTreeMap<&str, (&'static str, u8)>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    writeln!(out, "// {} шт.", chips.len()).expect("запись в String не отказывает");
    out.push_str("export const BANK_SWITCH = #{\n");
    for (suffix, (variant, bit)) in chips {
        writeln!(out, "    \"{suffix}\": \"{variant}:{bit}\",")
            .expect("запись в String не отказывает");
    }
    out.push_str("};\n");
    out
}

/// Только чипы, у которых `embassy-stm32` требует выбора банковой схемы
/// (несколько карт памяти) — для остальных фичи нет вовсе, и хук подставляет
/// пустую строку.
pub(crate) fn format_bank_modes(modes: &BTreeMap<&str, &'static str>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    writeln!(out, "// {} шт.", modes.len()).expect("запись в String не отказывает");
    out.push_str("export const BANK_MODE = #{\n");
    for (suffix, mode) in modes {
        writeln!(out, "    \"{suffix}\": \"{mode}\",").expect("запись в String не отказывает");
    }
    out.push_str("};\n");
    out
}

/// Штамп источников: [`source_stamp_versions`] и версия probe-rs — справочно,
/// она у каждого мейнтейнера своя.
pub(crate) fn format_source_stamp(
    embassy_version: &str,
    metapac_version: &str,
    probe_rs_version: &str,
) -> String {
    format!(
        "{}, probe-rs {probe_rs_version}\n",
        source_stamp_versions(embassy_version, metapac_version)
    )
}

/// Начало штампа, которое сверяет тест: объявленная версия `embassy-stm32` и
/// разрешённая `stm32-metapac`. Вторая нужна потому, что `embassy-stm32`
/// тянет metapac диапазоном (`"21"`), а lock-файла у генератора нет: минорный
/// релиз metapac молча меняет данные при той же версии `embassy-stm32`.
#[must_use]
pub fn source_stamp_versions(embassy_version: &str, metapac_version: &str) -> String {
    format!(
        "{SOURCE_STAMP_PREFIX}{embassy_version} (crates-cross/Cargo.toml), stm32-metapac {metapac_version}"
    )
}

pub(crate) fn format_chip_list(suffixes: &[&str], stamp: &str) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    writeln!(out, "// {} шт.", suffixes.len()).expect("запись в String не отказывает");
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

pub(crate) fn format_package_choices(choices: &BTreeMap<&str, Vec<String>>) -> String {
    let mut out = String::new();
    out.push_str(DATA_HEADER);
    writeln!(out, "// {} шт.", choices.len()).expect("запись в String не отказывает");
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
pub(crate) fn write_data_dir(dir: &Path, files: &[(String, String)]) -> anyhow::Result<()> {
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

/// Число как литерал Rust с разделителями разрядов (`131_072`): значение
/// уходит прямо в `const` crates-cross (`CONFIG_ERASE_SIZE` в `bsp::flash`), а
/// там `clippy::unreadable_literal` под `-D warnings` не пропускает шесть цифр
/// подряд. В `[env]` (`STATE_ERASE_SIZE`, `EXT_PAGE_SIZE`) `_` снимает фильтр
/// Liquid `remove`.
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

/// Одно поле Rhai-мапы. Экранирования в Rhai-строках здесь нет: все значения
/// — либо числа/адреса, либо тексты причин из `compute_ota_partitions`, где
/// кавычек нет по построению (проверяется `assert`, чтобы это не разъехалось
/// молча при правке сообщений).
fn push_field(out: &mut String, key: &str, value: &str) {
    assert!(!value.contains('"'), "кавычка в значении {key}: {value}");
    writeln!(out, "        \"{key}\": \"{value}\",").expect("запись в String не отказывает");
}

/// Файл раскладок, в котором лежит чип: первые два символа суффикса (`f4`,
/// `h7`, `wb`). Тот же ключ хук вычисляет в `import` — менять только вместе.
fn layout_family(suffix: &str) -> &str {
    &suffix[..2]
}

/// Поля одного набора `OtaPartitions` с префиксом `prefix` (`""` — схема без
/// настроек, `config_` — с ними); `page_size` — физическая страница чипа.
fn push_ota(out: &mut String, prefix: &str, p: &OtaPartitions, page_size: u64) {
    let mut field = |key: &str, value: &str| push_field(out, &format!("{prefix}{key}"), value);
    field("ota", "true");
    field("bootloader_length", &format_size(p.bootloader_length));
    field(
        "bootloader_state_origin",
        &format_addr(p.bootloader_state_origin),
    );
    field(
        "bootloader_state_length",
        &format_size(p.bootloader_state_length),
    );
    field(
        "bootloader_state_erase_size",
        &rust_literal(p.bootloader_state_erase_size),
    );
    field("swap_page_size", &p.swap_page_size.to_string());
    field(
        "swap_paged",
        if p.swap_page_size > page_size {
            "true"
        } else {
            "false"
        },
    );
    field("active_origin", &format_addr(p.active_origin));
    field("active_length", &format_size(p.active_length));
    field("dfu_origin", &format_addr(p.dfu_origin));
    field("dfu_length", &format_size(p.dfu_length));
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
        }
        Err(note) => {
            field("ota", "false");
            field("note", note);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_literal_groups_digits_by_three() {
        assert_eq!(rust_literal(128), "128");
        assert_eq!(rust_literal(16_384), "16_384");
        assert_eq!(rust_literal(131_072), "131_072");
        assert_eq!(rust_literal(1_048_576), "1_048_576");
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
}
