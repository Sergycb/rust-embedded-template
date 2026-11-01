//! Разбор метаданных `stm32-metapac`: карта памяти чипа и факты из общего
//! файла метаданных (блоки, интерфейсы, RNG, кэши, флаги сброса).

use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, bail};

/// Метки интерфейсов в таблице `INTERFACES` — в этом порядке.
pub(crate) const INTERFACE_TAGS: [&str; 6] = ["usart", "fdcan", "bxcan", "eth", "usb", "otg"];

/// Причины сброса в порядке проверки (имена — в `bsp::reset::take_cause`) и
/// поля флагов `RCC`, которые каждую означают. Флагов бывает несколько сразу,
/// поэтому порядок — от самого конкретного: флаг вывода (`PINRSTF`, у F2/F4/F7
/// `PADRSTF`) ставится при любом внутреннем сбросе и идёт последним, а питание
/// (`PORRSTF`, `BORRSTF`, `PWRRSTF` у G0/U0/C0, `V18PWRRSTF` у F0/F3) — перед
/// ним. Остальные флаги (`CPURSTF` и доменов у H7, `FWRSTF` у L0/L4, радио у
/// WL) причиной не считаются: их сброс заканчивается одним из этих.
const RESET_CAUSES: &[(&str, &[&str])] = &[
    ("LowPower", &["lpwrrstf"]),
    ("WindowWatchdog", &["wwdgrstf", "wwdg1rstf"]),
    ("IndependentWatchdog", &["iwdgrstf", "iwdg1rstf", "wdgrstf"]),
    ("Software", &["sftrstf"]),
    ("OptionBytes", &["oblrstf"]),
    ("PowerOn", &["porrstf", "pwrrstf", "borrstf", "v18pwrrstf"]),
    ("Pin", &["pinrstf", "padrstf"]),
];

/// Что добавить к `embassy_stm32::Config::default()`, чтобы у блока `RNG`
/// был клок: способ (`bsp::board` разворачивает его в строку конфига) и
/// вариант мультиплексора, если способ — мультиплексор.
pub(crate) type RngClock = (&'static str, &'static str);

/// Один регион памяти чипа — как в `stm32-metapac`, но только то, что нужно
/// для `memory.x`: имя (по нему `embassy-stm32` отличает одно- и двухбанковую
/// конфигурацию, см. `select_memory_config`), адрес, размер и (для flash)
/// реальный размер стирания и минимальную порцию записи.
pub(crate) struct RawRegion {
    pub(crate) name: String,
    pub(crate) kind: RegionKind,
    pub(crate) address: u64,
    pub(crate) size: u64,
    /// `Some` только для flash-регионов (`settings: Some(FlashSettings {..})`
    /// в исходнике `stm32-metapac`); у RAM/EEPROM-регионов — `None`.
    pub(crate) flash: Option<FlashSettings>,
}

/// Та часть `FlashSettings` из `stm32-metapac`, которая нужна шаблону.
///
/// Именованные поля, а не кортеж: третье значение (`erase_value`) не читается
/// по позиции никак, а два первых уже путались местами при чтении кода.
#[derive(Clone, Copy)]
pub(crate) struct FlashSettings {
    pub(crate) erase_size: u64,
    pub(crate) write_size: u64,
    /// Чем читается СТЁРТАЯ страница: `0xFF` у подавляющего большинства
    /// семейств, `0x00` у L0/L1 и STM32WB10CC/WB15CC.
    ///
    /// От этого зависит фича `flash-erase-zero` у `embassy-boot` (без неё
    /// обмен разделов читает признак «стёрто» наоборот) и то, работает ли
    /// раздел настроек вообще (`sequential-storage` ищет заголовок записи как
    /// «все байты `0xFF`»). Обе поломки молчаливые, поэтому значение берётся
    /// из метаданных, а не выводится по имени чипа.
    pub(crate) erase_value: u8,
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
pub(crate) enum RegionKind {
    Flash,
    Ram,
    /// L0/L1 — блок EEPROM, читается напрямую, пишется через контроллер flash.
    Eeprom,
}

/// Что генератор берёт из общего файла метаданных — того, который
/// `<chip>/metadata.rs` включает первой строкой
/// (`include!("../metadata_0180.rs")`): перечень блоков живёт там, а не в
/// файле чипа.
pub(crate) struct SharedMetadata {
    /// Имена блоков `IWDG*` — см. [`chip_watchdog`].
    pub(crate) watchdogs: Vec<String>,
    /// Блок `PWR` версии `h7rm0468` (STM32H723/725/730/733/735): только у этой
    /// линии потолок ядра на VOS0 зависит от option byte `CPU_FREQ_BOOST` —
    /// 520 МГц без него, 550 МГц с ним. По той же версии `embassy-stm32`
    /// выбирает предел частоты (`cfg(pwr_h7rm0468)` в `rcc/h.rs`), так что
    /// признак и HAL расходиться не могут.
    pub(crate) cpu_freq_boost: bool,
    /// Версия регистров блока `FLASH` (`g4c3`, `f4`, `l5`...).
    pub(crate) flash_version: Option<String>,
    /// Версия регистров блока `RCC` (`f4`, `h7`, `c0v2`...) — по ней
    /// [`reset_register`] находит флаги сброса.
    pub(crate) rcc_version: Option<String>,
    /// Интерфейсы связи, выведенные в корпусе, — см. [`parse_interfaces`].
    pub(crate) interfaces: Vec<&'static str>,
    /// Имена блоков, которыми `resources.rs` называет каждый интерфейс, — см.
    /// [`parse_instances`].
    pub(crate) instances: BTreeMap<&'static str, Vec<String>>,
    /// Прерывание блока `RNG` — см. [`parse_rng_interrupt`].
    pub(crate) rng_interrupt: Option<String>,
    /// Есть блок `CRS` — по нему `embassy-stm32` включает HSI48 (см. [`rng_clock`]).
    pub(crate) has_crs: bool,
    /// Есть блок `ICACHE` (L5, U3/U5, H5, WBA, H7R/S) — кэш инструкций вне ядра,
    /// включается битом `ICACHE_CR.EN`.
    has_icache: bool,
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
pub(crate) fn parse_chip_memory(
    chips_dir: &Path,
    suffix: &str,
) -> anyhow::Result<Vec<Vec<RawRegion>>> {
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
pub(crate) fn chip_watchdog(
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

/// Путь к общему файлу метаданных чипа и разобранные из него факты. Общий файл
/// делят десятки чипов, поэтому разбор кешируется в `cache` по пути.
pub(crate) fn shared_metadata<'c>(
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
                rcc_version: registers_version(&shared_text, "rcc"),
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
pub(crate) fn chip_caches(
    chips_dir: &Path,
    shared: &SharedMetadata,
) -> anyhow::Result<Vec<&'static str>> {
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

/// Регистр флагов сброса в файле регистров `RCC` (`rcc_<версия>.rs` в
/// `stm32-metapac`) и его флаги парами `поле[,поле]:Причина` через пробел в
/// порядке [`RESET_CAUSES`].
///
/// Регистр — тот, у которого есть `set_rmvf` (снять флаги), и имя его
/// аксессора в блоке `Rcc`: `csr` у большинства, `csr2` у C0, `rsr` у H5/H7.
/// Аксессоры ядер двухъядерных H7 (`c1_rsr`) отбрасываются — общий регистр
/// тот же.
///
/// # Errors
///
/// Нет регистра с `set_rmvf`, их больше одного или ни один из известных
/// флагов не нашёлся: формат `stm32-metapac` сменился, и тихо оставить чип
/// без причины сброса было бы хуже, чем упасть.
pub(crate) fn reset_register(text: &str) -> anyhow::Result<(String, String)> {
    let mut found = Vec::new();
    for chunk in text.split("pub struct ").skip(1) {
        let Some((name, _)) = chunk.split_once('(') else {
            continue;
        };
        // Тело структуры — до следующей; `impl Default` и `defmt::Format`
        // идут за ним, полей там нет.
        if !chunk.contains("pub fn set_rmvf(") {
            continue;
        }
        let pattern = format!("-> crate::common::Reg<regs::{name}, ");
        for line in text.lines().filter(|line| line.contains(&pattern)) {
            let accessor = line
                .trim_start()
                .strip_prefix("pub const fn ")
                .and_then(|rest| rest.split_once("(self)"))
                .map(|(accessor, _)| accessor);
            if let Some(accessor) = accessor
                && !accessor.starts_with("c1_")
                && !accessor.starts_with("c2_")
            {
                found.push((accessor.to_owned(), chunk));
            }
        }
    }
    let [(accessor, chunk)] = found.as_slice() else {
        anyhow::bail!(
            "ожидался один регистр RCC с set_rmvf, найдено: {:?}",
            found.iter().map(|(a, _)| a).collect::<Vec<_>>()
        );
    };
    let has = |field: &str| chunk.contains(&format!("pub const fn {field}(&self) -> bool"));
    let pairs: Vec<String> = RESET_CAUSES
        .iter()
        .filter_map(|(cause, fields)| {
            let present: Vec<&str> = fields.iter().copied().filter(|f| has(f)).collect();
            (!present.is_empty()).then(|| format!("{}:{cause}", present.join(",")))
        })
        .collect();
    anyhow::ensure!(
        !pairs.is_empty(),
        "в регистре {accessor} нет ни одного известного флага сброса"
    );
    Ok((accessor.clone(), pairs.join(" ")))
}

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
pub(crate) fn rng_clock(suffix: &str) -> Option<RngClock> {
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
pub(crate) fn select_memory_config(
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

/// Метка и имя блока, если блок — шина внешней флеши с выводами одной
/// микросхемы в корпусе: у `spi` — `SCK`, `MISO`, `MOSI`, у `quadspi` — `CLK`,
/// `BK1_NCS` и `BK1_IO0..3` (первый банк), у `octospi` — `CLK`, `NCS` и
/// `IO0..3`. У чипов с мультиплексором `OCTOSPIM` (H72x/H73x, L4+, U5) своих
/// выводов у `OCTOSPI*` нет — они у мультиплексора, `mux_pins`, с приставкой
/// порта (`P1_`, `P2_`), и годится любой порт целиком. Не интерфейс связи: в
/// меню транспортов эти метки не попадают, только в [`parse_instances`] — по
/// ним хук выбирает шину флеши.
fn flash_bus_block<'b>(block: &'b str, mux_pins: &str) -> Option<(&'static str, &'b str)> {
    let (tag, prefixes, signals): (&'static str, &[&str], &[&str]) =
        match field(block, "kind").map(|v| v.trim_matches('"')) {
            Some("spi") => ("spi", &[""], &["SCK", "MISO", "MOSI"]),
            Some("quadspi") => ("quadspi", &["BK1_"], &["NCS", "IO0", "IO1", "IO2", "IO3"]),
            Some("octospi") => (
                "octospi",
                &["", "P1_", "P2_"],
                &["CLK", "NCS", "IO0", "IO1", "IO2", "IO3"],
            ),
            _ => return None,
        };
    let own = block_pins(block);
    let pins = if tag == "octospi" && own.trim().is_empty() {
        mux_pins
    } else {
        own
    };
    let has = |signal: &str| pins.contains(&format!("signal: \"{signal}\""));
    // `CLK` у QUADSPI общий на оба банка — без приставки.
    let wired = (tag != "quadspi" || has("CLK"))
        && prefixes.iter().any(|prefix| {
            signals
                .iter()
                .all(|signal| has(&format!("{prefix}{signal}")))
        });
    let name = field(block, "name").map(|v| v.trim_matches('"'))?;
    wired.then_some((tag, name))
}

/// Список выводов блока метаданных — текст между `pins: &[` и `]`.
fn block_pins(block: &str) -> &str {
    block.split_once("pins: &[").map_or("", |(_, rest)| {
        rest.split_once(']').map_or(rest, |(p, _)| p)
    })
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
/// блок, который проект получает по умолчанию. Шины внешней флеши
/// ([`flash_bus_block`]) — там же, под своими метками.
fn parse_instances(text: &str) -> BTreeMap<&'static str, Vec<String>> {
    let mut ranked: BTreeMap<&'static str, Vec<(u8, usize, String)>> = BTreeMap::new();
    let mux_pins = text
        .split("Peripheral {")
        .skip(1)
        .find(|block| field(block, "kind").is_some_and(|v| v.trim_matches('"') == "octospim"))
        .map_or("", block_pins);
    for block in text.split("Peripheral {").skip(1) {
        let Some((tag, name)) = interface_block(block).or_else(|| flash_bus_block(block, mux_pins))
        else {
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

/// Вид региона по полю `kind` — строго, без «прочее».
///
/// Сопоставление точное (после префикса `MemoryRegionKind::`): оно не
/// переживает переименование варианта: `MemoryRegionKind::FlashBank1` дал бы «прочее», а
/// [`compute_memory_layout`](crate::layout::compute_memory_layout) отбирает flash-регионы по `kind == Flash` — и
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

    /// Кусок `rcc_<версия>.rs` из `stm32-metapac`: аксессоры блока `Rcc` и
    /// структуры регистров с полями-флагами — как их пишет генератор PAC.
    fn rcc_excerpt(accessors: &[(&str, &str)], registers: &[(&str, &[&str])]) -> String {
        let mut text = String::from("pub struct Rcc {\n    ptr: *mut u8,\n}\nimpl Rcc {\n");
        for (accessor, register) in accessors {
            text.push_str(&format!(
                "    pub const fn {accessor}(self) -> crate::common::Reg<regs::{register}, \
                 crate::common::RW> {{\n    }}\n"
            ));
        }
        text.push_str("}\npub mod regs {\n");
        for (register, fields) in registers {
            text.push_str(&format!(
                "    pub struct {register}(pub u32);\n    impl {register} {{\n"
            ));
            for field in *fields {
                text.push_str(&format!(
                    "        pub const fn {field}(&self) -> bool {{\n        }}\n        \
                     pub fn set_{field}(&mut self, val: bool) {{\n        }}\n"
                ));
            }
            text.push_str("    }\n");
        }
        text.push_str("}\n");
        text
    }

    /// F4: `CSR`, сторож — `WDGRSTF`, вывод — `PADRSTF`, питание — `POR`/`BOR`
    /// одной причиной.
    #[test]
    fn reset_flags_of_f4() {
        let text = rcc_excerpt(
            &[("cr", "Cr"), ("csr", "Csr")],
            &[
                ("Cr", &["hsion"]),
                (
                    "Csr",
                    &[
                        "lsion", "rmvf", "borrstf", "padrstf", "porrstf", "sftrstf", "wdgrstf",
                        "wwdgrstf", "lpwrrstf",
                    ],
                ),
            ],
        );
        let (register, flags) = reset_register(&text).unwrap();
        assert_eq!(register, "csr");
        assert_eq!(
            flags,
            "lpwrrstf:LowPower wwdgrstf:WindowWatchdog wdgrstf:IndependentWatchdog sftrstf:Software porrstf,borrstf:PowerOn padrstf:Pin"
        );
    }

    /// H743: `RSR`, а не регистр ядра `C1_RSR` той же структуры полей;
    /// сторожа — `IWDG1`/`WWDG1`, флаги доменов причиной не считаются.
    #[test]
    fn reset_flags_of_h743_skip_the_core_alias() {
        let fields: &[&str] = &[
            "rmvf",
            "cpurstf",
            "d1rstf",
            "d2rstf",
            "borrstf",
            "pinrstf",
            "porrstf",
            "sftrstf",
            "iwdg1rstf",
            "wwdg1rstf",
            "lpwrrstf",
        ];
        let text = rcc_excerpt(
            &[("rsr", "Rsr"), ("c1_rsr", "C1Rsr")],
            &[("C1Rsr", fields), ("Rsr", fields)],
        );
        let (register, flags) = reset_register(&text).unwrap();
        assert_eq!(register, "rsr");
        assert_eq!(
            flags,
            "lpwrrstf:LowPower wwdg1rstf:WindowWatchdog iwdg1rstf:IndependentWatchdog sftrstf:Software porrstf,borrstf:PowerOn pinrstf:Pin"
        );
    }

    /// C0: регистр `CSR2` (в `CSR1` — LSE), питание — `PWRRSTF`; `CSR2`
    /// не путается с `CSR`-подобными именами.
    #[test]
    fn reset_flags_of_c0() {
        let text = rcc_excerpt(
            &[("csr1", "Csr1"), ("csr2", "Csr2")],
            &[
                ("Csr1", &["lseon"]),
                (
                    "Csr2",
                    &[
                        "lsion", "rmvf", "oblrstf", "pinrstf", "pwrrstf", "sftrstf", "iwdgrstf",
                        "wwdgrstf", "lpwrrstf",
                    ],
                ),
            ],
        );
        let (register, flags) = reset_register(&text).unwrap();
        assert_eq!(register, "csr2");
        assert_eq!(
            flags,
            "lpwrrstf:LowPower wwdgrstf:WindowWatchdog iwdgrstf:IndependentWatchdog sftrstf:Software oblrstf:OptionBytes pwrrstf:PowerOn pinrstf:Pin"
        );
    }

    /// G0: `CSR` с `PWRRSTF` и `OBLRSTF`.
    #[test]
    fn reset_flags_of_g0() {
        let text = rcc_excerpt(
            &[("csr", "Csr")],
            &[(
                "Csr",
                &[
                    "lsion", "rmvf", "oblrstf", "pinrstf", "pwrrstf", "sftrstf", "iwdgrstf",
                    "wwdgrstf", "lpwrrstf",
                ],
            )],
        );
        let (register, flags) = reset_register(&text).unwrap();
        assert_eq!(register, "csr");
        assert!(
            flags.ends_with("oblrstf:OptionBytes pwrrstf:PowerOn pinrstf:Pin"),
            "{flags}"
        );
    }

    /// Регистра с `set_rmvf` нет или их два — громкая ошибка, а не чип без
    /// причины сброса.
    #[test]
    fn reset_register_must_be_unique() {
        let none = rcc_excerpt(&[("csr", "Csr")], &[("Csr", &["lsion", "porrstf"])]);
        assert!(reset_register(&none).is_err());
        let two = rcc_excerpt(
            &[("hwrsr", "Hwrsr"), ("rsr", "Rsr")],
            &[
                ("Hwrsr", &["rmvf", "porrstf"]),
                ("Rsr", &["rmvf", "porrstf"]),
            ],
        );
        assert!(reset_register(&two).is_err());
        let unknown = rcc_excerpt(&[("csr", "Csr")], &[("Csr", &["rmvf", "fwrstf"])]);
        assert!(reset_register(&unknown).is_err());
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

    /// Шины внешней флеши — в `INSTANCES` под своими метками, но не в
    /// интерфейсах связи; годится блок со всеми выводами одной микросхемы.
    #[test]
    fn flash_buses_need_the_pins_of_one_chip() {
        let block = |name: &str, kind: &str, pins: &str| {
            format!(
                "    Peripheral {{
        name: \"{name}\",
        registers: Some(PeripheralRegisters {{
                             kind: \"{kind}\",
            version: \"v1\",
        }}),
                         pins: &[{pins}],
    }},
"
            )
        };
        let pins = |signals: &[&str]| {
            signals
                .iter()
                .map(|signal| {
                    format!(
                        "
            PeripheralPin {{ pin: \"PB1\", signal: \"{signal}\", af: Some(9) }},
        "
                    )
                })
                .collect::<String>()
        };
        let spi = pins(&["SCK", "MISO", "MOSI"]);
        let quad = pins(&["CLK", "BK1_NCS", "BK1_IO0", "BK1_IO1", "BK1_IO2", "BK1_IO3"]);
        let text = [
            block("SPI2", "spi", &spi),
            block("SPI1", "spi", &spi),
            block("SPI3", "spi", &pins(&["SCK", "MOSI"])),
            block("QUADSPI", "quadspi", &quad),
            block("OCTOSPI1", "octospi", &pins(&["CLK", "NCS", "IO0", "IO1"])),
        ]
        .concat();
        let found = parse_instances(&text);
        assert_eq!(found["spi"], ["SPI1", "SPI2"], "SPI3 без MISO не годится");
        assert_eq!(found["quadspi"], ["QUADSPI"]);
        assert!(!found.contains_key("octospi"), "OCTOSPI1 без IO2/IO3");
        assert!(parse_interfaces(&text).is_empty());

        // С мультиплексором выводы у `OCTOSPIM`, с приставкой порта: годится
        // порт целиком, а порты вперемешку — нет.
        let port = |p: &str| ["CLK", "NCS", "IO0", "IO1", "IO2", "IO3"].map(|s| format!("{p}{s}"));
        let with_mux = |mux: &[String]| {
            let mux: Vec<&str> = mux.iter().map(String::as_str).collect();
            [
                block("OCTOSPI1", "octospi", ""),
                block("OCTOSPIM", "octospim", &pins(&mux)),
            ]
            .concat()
        };
        assert_eq!(
            parse_instances(&with_mux(&port("P2_")))["octospi"],
            ["OCTOSPI1"]
        );
        let mut mixed = port("P1_");
        mixed[5] = "P2_IO3".to_owned();
        assert!(!parse_instances(&with_mux(&mixed)).contains_key("octospi"));
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
}
