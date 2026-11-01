//! Откуда генератор берёт данные: репозиторий шаблона, `cargo metadata` по
//! временной копии с подставленными плейсхолдерами (`PatchedRepoCopy`,
//! `strip_liquid`) и `probe-rs chip list`.

use std::{
    collections::BTreeSet,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, bail};

/// Адрес rust-lib, которым заменяется плейсхолдер `{{rust_lib}}` в
/// `crates-cross/Cargo.toml` (без `.git`, как его хранит хук): с ним как есть
/// `cargo metadata` падает на «invalid url». Его же подставляет при генерации
/// `template-check` (`REPO_DEFAULTS`).
pub const RUST_LIB: &str = "https://github.com/Sergycb/rust-lib";

/// Любая реальная чип-фича годится — нужна только чтобы `cargo metadata` смог
/// разрешить зависимость `embassy-stm32`: в самом `crates-cross/Cargo.toml` вместо
/// неё стоит нерезолвящийся плейсхолдер `{{chip_feature}}`, см.
/// `PatchedRepoCopy`.
const PLACEHOLDER_CHIP_FEATURE: &str = "stm32f407ve";

/// Суффиксы после дефиса, обозначающие конкретное ядро (двухъядерные чипы) —
/// не корпусировку/градацию. Должно совпадать с `core_override()` в
/// `chip-select.rhai`.
const CORE_MARKERS: &[&str] = &["cm0", "cm0p", "cm3", "cm4", "cm7", "cm23", "cm33"];

/// Начало условного тега. Не `{% if %}` целиком: между `if` и `%}` стоит само
/// условие.
const LIQUID_IF: &str = "{% if";
const LIQUID_ENDIF: &str = "{% endif %}";
const LIQUID_ELSE: &str = "{% else %}";

/// Результат разбора `cargo metadata` для пропатченного `crates-cross/Cargo.toml`:
/// всё, что нужно взять из графа зависимостей за один вызов `cargo`.
pub(crate) struct CargoMetadata {
    /// Полный список объявленных чип-фич `embassy-stm32` (например
    /// `stm32f407ve`, а у двухъядерных чипов и силиконовых градаций — с
    /// суффиксом через дефис: `stm32h745zi-cm7`, `stm32l151c6-a`) — это
    /// декларированные features пакета, а не только разрешённые для текущей
    /// сборки. Отобраны только фичи вида `stm32` + строчные буквы/цифры,
    /// максимум с одним дефисом-разделителем (у всех прочих фич пакета либо
    /// нет префикса `stm32`, либо есть небуквенные символы помимо дефиса).
    pub(crate) embassy_chip_features: BTreeSet<String>,
    /// Каталог `src/chips` в исходниках `stm32-metapac` (той версии, что
    /// реально разрешилась для `embassy-stm32`) — там лежит подкаталог на
    /// каждый чип с его `metadata.rs` (адреса/размеры flash и RAM,
    /// см. `parse_chip_memory`). `embassy-stm32` уже зависит от него с
    /// фичей `metadata` — отдельно эту зависимость никуда добавлять не
    /// нужно, только найти, куда Cargo её скачал.
    pub(crate) stm32_metapac_chips_dir: PathBuf,
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
        let rev = rust_lib_rev(&dir)?;
        strip_liquid_from_manifests(&dir, &rev)?;

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

/// Корень репозитория шаблона — каталог, в котором лежит `chip-data-gen/`.
pub fn repo_root() -> PathBuf {
    let mut dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    dir.pop();
    dir
}

/// Версия `embassy-stm32` так, как она объявлена в `crates-cross/Cargo.toml` — не
/// разрешённая cargo: сверять надо именно объявление, иначе безобидный
/// патч-релиз в реестре ронял бы тест штампа.
pub fn declared_embassy_version(repo_root: &Path) -> anyhow::Result<String> {
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

pub(crate) fn resolve_cargo_metadata(repo_root: &Path) -> anyhow::Result<CargoMetadata> {
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

/// Часть суффикса, которую нужно сверять с `probe-rs chip list` — до первого
/// дефиса и в верхнем регистре. probe-rs не различает ядро/градацию в имени
/// цели (`STM32H745ZI` — одна цель с двумя ядрами внутри, без `-CM7`), это
/// различие есть только в фиче `embassy-stm32`, которая и определяет, какой
/// PAC-подмодуль/ядро реально соберётся.
pub(crate) fn probe_rs_candidate(suffix: &str) -> String {
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
pub(crate) fn package_candidates(suffix: &str, probe_rs_chips: &BTreeSet<String>) -> Vec<String> {
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

pub(crate) fn probe_rs_version() -> anyhow::Result<String> {
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

/// Множество идентификаторов чипов, которые знает локально установленный
/// `probe-rs` (`probe-rs chip list`), например `STM32F407VE` — без суффикса
/// корпус/темп.диапазон (`Tx`/`Hx`/...), см. обоснование в MAINTAINING.md
/// («Плейсхолдеры шаблона»).
pub(crate) fn probe_rs_chip_names() -> anyhow::Result<BTreeSet<String>> {
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

/// Убирает Liquid-условия (`{% if ... %}...{% endif %}`) из всех `Cargo.toml`
/// временной копии, выбирая ЛОЖНУЮ ветку — то есть выбрасывая блок целиком.
///
/// Нужно, потому что `cargo metadata` читает манифесты всех членов workspace,
/// а те содержат условные куски (`"dual-core"` в `crates-cross/bsp`/`crates-cross/boot`,
/// `"boot"` среди `members` и фича банка в `crates-cross/Cargo.toml`) — с ними это
/// не TOML. Ложная ветка безопаснее истинной: она оставляет манифест
/// минимальным, а всё, что нужно генератору (`embassy-stm32` и его
/// `stm32-metapac`), объявлено безусловно.
fn strip_liquid_from_manifests(dir: &Path, rev: &str) -> anyhow::Result<()> {
    for entry in fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        if entry.file_type()?.is_dir() {
            strip_liquid_from_manifests(&path, rev)?;
        } else if entry.file_name() == "Cargo.toml" {
            let original = fs::read_to_string(&path)?;
            // `unless` стриппер не понимает: его теги остались бы в TOML, и
            // `cargo metadata` упал бы на «invalid key-value pair» без намёка
            // на причину. В манифестах его нет; появится — учить стриппер.
            anyhow::ensure!(
                !original.contains("{% unless") && !original.contains("{%- unless"),
                "{}: `{{% unless %}}` в манифесте — strip_liquid его не понимает",
                path.display(),
            );
            let stripped = strip_liquid(&original)
                .replace("{{rust_lib}}", RUST_LIB)
                .replace("{{rust_lib_rev}}", rev);
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
        // Ложная ветка — то, что за `{% else %}` этого уровня: без неё
        // `members` в `crates-cross/Cargo.toml` (`boot` — только с OTA)
        // пропадал целиком, и `cargo metadata` видел пустой workspace.
        if let Some(otherwise) = top_level_else(&after[..end]) {
            out.push_str(&strip_liquid(&after[otherwise..end - LIQUID_ENDIF.len()]));
        }
        rest = &after[end..];
    }
    out.push_str(rest);
    out
}

/// Смещение сразу за `{% else %}` уровня `{% if %}` в начале `block` (блок —
/// до парного `{% endif %}` включительно); `None` — ветки `else` нет.
fn top_level_else(block: &str) -> Option<usize> {
    let mut depth = 0usize;
    let mut at = 0;
    loop {
        let next = [LIQUID_IF, LIQUID_ELSE, LIQUID_ENDIF]
            .into_iter()
            .filter_map(|tag| block[at..].find(tag).map(|offset| (at + offset, tag)))
            .min_by_key(|&(offset, _)| offset)?;
        match next {
            (offset, LIQUID_ELSE) if depth == 1 => return Some(offset + LIQUID_ELSE.len()),
            (_, LIQUID_IF) => depth += 1,
            (_, LIQUID_ENDIF) => depth -= 1,
            _ => {}
        }
        at = next.0 + next.1.len();
    }
}

/// Ревизия rust-lib для плейсхолдера `{{rust_lib_rev}}` — та же, что у
/// рабочей строки `supervisor` в корневом `Cargo.toml` шаблона: без своей
/// копии числа, которую пришлось бы менять вместе с ним. С плейсхолдером как
/// есть `cargo metadata` искал бы несуществующую ревизию.
fn rust_lib_rev(root: &Path) -> anyhow::Result<String> {
    let manifest = fs::read_to_string(root.join("Cargo.toml"))?;
    manifest
        .lines()
        .filter(|line| line.starts_with("supervisor") && line.contains("rust-lib.git"))
        .find_map(|line| line.split("rev = \"").nth(1)?.split('"').next())
        .map(str::to_owned)
        .context("в корневом Cargo.toml нет строки `supervisor` с `rev` rust-lib")
}

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

#[cfg(test)]
mod tests {
    use super::*;

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

    /// Из `if`/`else` остаётся ветка `else` — со своими вложенными условиями,
    /// выброшенными так же; `else` вложенного блока ветку не делит.
    #[test]
    fn the_else_branch_is_kept() {
        let text = "a\n{%- if x %}\nb\n{% if y %}c{% else %}d{% endif %}\n{%- else %}\n\
                    e{% if z %}f{% endif %}\n{%- endif %}\ng\n";
        assert_eq!(strip_liquid(text), "a\n\ne\n\ng\n");
    }

    /// `elsif` — часть ложной ветки: при всех ложных условиях Liquid берёт
    /// `else`, и стриппер — тоже (как в `crates-cross/bsp/Cargo.toml`).
    #[test]
    fn an_elsif_falls_through_to_else() {
        let text = "a\n{% if x %}b{% elsif y %}c{% else %}d{% endif %}\ne\n";
        assert_eq!(strip_liquid(text), "a\nd\ne\n");
        let text = "a\n{% if x %}b{% elsif y %}c{% endif %}\ne\n";
        assert_eq!(strip_liquid(text), "a\n\ne\n");
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
